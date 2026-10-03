import pytest
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

from core.exceptions import (
    ExternalServiceError,
    RangeNotSatisfiableError,
    ResourceNotFoundError,
)
from services.local_files_service import LocalFilesService


def _make_mock_cache() -> AsyncMock:
    cache = AsyncMock()
    cache.get = AsyncMock(return_value=None)
    cache.set = AsyncMock()
    return cache


def _make_preferences(library_paths: list[str] | None = None) -> MagicMock:
    prefs = MagicMock()
    paths = library_paths if library_paths is not None else ["/music"]
    prefs.get_typed_library_settings.return_value = SimpleNamespace(
        library_roots=[SimpleNamespace(path=path) for path in paths]
    )
    advanced = MagicMock()
    advanced.cache_ttl_local_files_recently_added = 120
    prefs.get_advanced_settings.return_value = advanced
    return prefs


def _native_album(**overrides) -> SimpleNamespace:
    base = dict(
        release_group_mbid="rg-1",
        album_title="Test Album",
        album_artist_name="Test Artist",
        track_count=3,
        total_size_bytes=1000,
        quality_format="flac",
        year=2024,
        is_compilation=False,
        cover_url=None,
        last_imported_at=1700000000.0,
    )
    base.update(overrides)
    return SimpleNamespace(**base)


def _native_track(**overrides) -> SimpleNamespace:
    base = dict(
        id="file-uuid-1",
        recording_mbid=None,
        disc_number=1,
        track_number=1,
        track_title="Test Track",
        artist_name="Test Artist",
        file_path="/music/a/b/track.flac",
        file_format="flac",
        bit_rate=1000,
        sample_rate=44100,
        bit_depth=16,
        duration_seconds=180.0,
        file_size_bytes=5000,
    )
    base.update(overrides)
    return SimpleNamespace(**base)


@pytest.fixture
def service(tmp_path):
    music_dir = tmp_path / "music"
    music_dir.mkdir()
    library_repo = AsyncMock()
    prefs = _make_preferences([str(music_dir)])
    cache = _make_mock_cache()
    svc = LocalFilesService(
        library_repo=library_repo,
        preferences_service=prefs,
        cache=cache,
    )
    return svc, library_repo, music_dir, cache


@pytest.mark.asyncio
async def test_stream_track_validates_audio_format(service):
    svc, library_repo, music_dir, cache = service
    bad_file = music_dir / "test.txt"
    bad_file.write_text("not audio")

    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": str(bad_file)}
    )

    with pytest.raises(ExternalServiceError, match="Unsupported audio format"):
        await svc.stream_track("f1")


@pytest.mark.asyncio
async def test_stream_track_serves_valid_file(service):
    svc, library_repo, music_dir, cache = service
    audio_file = music_dir / "song.flac"
    audio_file.write_bytes(b"fLaC" + b"\x00" * 100)

    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": str(audio_file)}
    )

    chunks_iter, headers, status = await svc.stream_track("f1")
    assert status == 200
    assert headers["Content-Type"] == "audio/flac"

    collected = b""
    async for chunk in chunks_iter:
        collected += chunk
    assert len(collected) == 104


@pytest.mark.asyncio
async def test_stream_track_handles_range_request(service):
    svc, library_repo, music_dir, cache = service
    audio_file = music_dir / "song.mp3"
    audio_file.write_bytes(b"\xff\xfb" + b"\x00" * 998)

    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": str(audio_file)}
    )

    chunks_iter, headers, status = await svc.stream_track(
        "f1", range_header="bytes=0-99"
    )
    assert status == 206
    assert "Content-Range" in headers

    collected = b""
    async for chunk in chunks_iter:
        collected += chunk
    assert len(collected) == 100


@pytest.mark.asyncio
async def test_stream_track_raises_on_missing_file(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": str(music_dir / "nonexistent.flac")}
    )

    with pytest.raises(ResourceNotFoundError, match="not found"):
        await svc.stream_track("f1")


@pytest.mark.asyncio
async def test_stream_track_raises_on_unknown_file_id(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_file_row_by_id = AsyncMock(return_value=None)

    with pytest.raises(ResourceNotFoundError, match="not found"):
        await svc.stream_track("missing")


@pytest.mark.asyncio
async def test_stream_track_raises_on_path_traversal(service):
    svc, library_repo, music_dir, cache = service
    traversal_path = str(music_dir / ".." / ".." / "etc" / "passwd")
    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": traversal_path}
    )

    with pytest.raises(PermissionError, match="outside library directories"):
        await svc.stream_track("f1")


@pytest.mark.asyncio
async def test_stream_track_handles_suffix_range(service):
    svc, library_repo, music_dir, cache = service
    audio_file = music_dir / "song.mp3"
    audio_file.write_bytes(b"\xff\xfb" + b"\x00" * 998)

    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": str(audio_file)}
    )

    chunks_iter, headers, status = await svc.stream_track(
        "f1", range_header="bytes=-200"
    )
    assert status == 206
    assert "Content-Range" in headers

    collected = b""
    async for chunk in chunks_iter:
        collected += chunk
    assert len(collected) == 200


@pytest.mark.asyncio
async def test_stream_track_rejects_malformed_range(service):
    svc, library_repo, music_dir, cache = service
    audio_file = music_dir / "song.mp3"
    audio_file.write_bytes(b"\xff\xfb" + b"\x00" * 998)

    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": str(audio_file)}
    )

    with pytest.raises(RangeNotSatisfiableError) as exc:
        await svc.stream_track("f1", range_header="bytes=abc-xyz")
    assert exc.value.file_size == 1000


@pytest.mark.asyncio
async def test_stream_track_rejects_invalid_range(service):
    svc, library_repo, music_dir, cache = service
    audio_file = music_dir / "song.mp3"
    audio_file.write_bytes(b"\xff\xfb" + b"\x00" * 98)

    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": str(audio_file)}
    )

    with pytest.raises(RangeNotSatisfiableError) as exc:
        await svc.stream_track("f1", range_header="bytes=5000-6000")
    assert exc.value.file_size == 100


@pytest.mark.asyncio
@pytest.mark.parametrize("range_header", ["bytes=0-1,4-5", "items=0-1", "bytes=-0"])
async def test_stream_track_rejects_unsupported_range_forms(service, range_header):
    svc, library_repo, music_dir, _cache = service
    audio_file = music_dir / "song.mp3"
    audio_file.write_bytes(b"\xff\xfb" + b"\x00" * 98)
    library_repo.get_file_row_by_id = AsyncMock(
        return_value={"file_path": str(audio_file)}
    )
    with pytest.raises(RangeNotSatisfiableError):
        await svc.stream_track("f1", range_header=range_header)


@pytest.mark.asyncio
async def test_get_albums_maps_native_albums(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_albums_page = AsyncMock(return_value=([_native_album()], 1))

    result = await svc.get_albums(limit=10, offset=0)

    assert result.total == 1
    assert result.items[0].musicbrainz_id == "rg-1"
    assert result.items[0].name == "Test Album"
    assert result.items[0].artist_name == "Test Artist"
    assert result.items[0].primary_format == "flac"
    library_repo.get_albums_page.assert_awaited_once()


@pytest.mark.asyncio
async def test_get_albums_translates_pagination_and_sort(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_albums_page = AsyncMock(return_value=([], 0))

    await svc.get_albums(limit=20, offset=40, sort_by="date_added")

    _, kwargs = library_repo.get_albums_page.call_args
    assert kwargs["page"] == 3
    assert kwargs["page_size"] == 20
    assert kwargs["sort"] == "recent"


@pytest.mark.asyncio
async def test_match_album_by_mbid_maps_native_tracks(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_tracks = AsyncMock(
        return_value=[
            _native_track(id="t1", file_size_bytes=100),
            _native_track(id="t2", file_size_bytes=200),
        ]
    )

    match = await svc.match_album_by_mbid("rg-1")

    assert match.found is True
    assert match.musicbrainz_id == "rg-1"
    assert [t.track_file_id for t in match.tracks] == ["t1", "t2"]
    assert match.total_size_bytes == 300
    assert match.primary_format == "flac"


@pytest.mark.asyncio
async def test_match_album_by_mbid_not_found_when_no_tracks(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_tracks = AsyncMock(return_value=[])

    match = await svc.match_album_by_mbid("rg-empty")
    assert match.found is False


@pytest.mark.asyncio
async def test_match_album_by_mbid_exposes_recording_mbid(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_tracks = AsyncMock(
        return_value=[_native_track(id="t1", recording_mbid="rec-1")]
    )

    match = await svc.match_album_by_mbid("rg-1")
    assert match.tracks[0].recording_mbid == "rec-1"


@pytest.mark.asyncio
async def test_match_recordings_by_mbid_batches_and_normalizes(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_library_files_for_recordings = AsyncMock(
        return_value={
            "rec-a": [_native_track(id="file-a", track_title="A")],
            "rec-b": [],
        }
    )

    result = await svc.match_recordings_by_mbid(["REC-A", "rec-b", "  "])

    library_repo.get_library_files_for_recordings.assert_awaited_once_with(
        ["rec-a", "rec-b"]
    )
    assert result == {"rec-a": ("A", "file-a")}


@pytest.mark.asyncio
async def test_match_recordings_by_mbid_empty_input_skips_repo(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_library_files_for_recordings = AsyncMock()

    assert await svc.match_recordings_by_mbid([]) == {}
    library_repo.get_library_files_for_recordings.assert_not_called()


@pytest.mark.asyncio
async def test_get_album_tracks_by_id_uses_native_tracks(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_tracks = AsyncMock(return_value=[_native_track(id="t9")])

    tracks = await svc.get_album_tracks_by_id("rg-1")
    assert [t.track_file_id for t in tracks] == ["t9"]


@pytest.mark.asyncio
async def test_get_recently_added_uses_cache(service):
    svc, library_repo, music_dir, cache = service
    cache.get = AsyncMock(
        return_value=[
            {
                "musicbrainz_id": "mbid-10",
                "name": "Cached Album",
                "artist_name": "Cached Artist",
                "track_count": 12,
                "total_size_bytes": 123456,
                "artist_mbid": None,
                "year": 2024,
                "primary_format": "flac",
                "cover_url": None,
                "date_added": "2026-02-17T00:00:00Z",
            }
        ]
    )
    library_repo.get_albums_page = AsyncMock(return_value=([], 0))

    result = await svc.get_recently_added(limit=20)

    assert len(result) == 1
    assert result[0].name == "Cached Album"
    library_repo.get_albums_page.assert_not_awaited()


@pytest.mark.asyncio
async def test_get_recently_added_caches_result(service):
    svc, library_repo, music_dir, cache = service
    cache.get = AsyncMock(return_value=None)
    library_repo.get_albums_page = AsyncMock(
        return_value=(
            [
                _native_album(
                    release_group_mbid="mbid-123", album_title="Album From Library"
                )
            ],
            1,
        )
    )

    result = await svc.get_recently_added(limit=20)

    assert len(result) == 1
    assert result[0].musicbrainz_id == "mbid-123"
    library_repo.get_albums_page.assert_awaited_once()
    _, kwargs = library_repo.get_albums_page.call_args
    assert kwargs["sort"] == "recent"
    cache.set.assert_called()
    cache_key = cache.set.call_args[0][0]
    assert cache_key == "local_files_recently_added:20"


@pytest.mark.asyncio
async def test_get_storage_stats_sources_counts_from_library(service):
    # Counts come from the library aggregate (get_stats); only disk-free is a real fs read.
    svc, library_repo, music_dir, cache = service
    library_repo.get_stats = AsyncMock(
        return_value=SimpleNamespace(
            total_tracks=1685,
            total_albums=177,
            total_artists=85,
            total_size_bytes=1_000_000,
            format_breakdown={"flac": 1092, "mp3": 544},
        )
    )

    stats = await svc.get_storage_stats()

    assert stats.total_tracks == 1685
    assert stats.total_albums == 177
    assert stats.total_artists == 85
    assert stats.total_size_bytes == 1_000_000
    assert stats.total_size_human  # humanised, non-empty
    assert stats.disk_free_bytes > 0  # real disk_usage on the tmp root
    assert stats.format_breakdown["flac"].count == 1092
    assert stats.format_breakdown["mp3"].count == 544
    library_repo.get_stats.assert_awaited_once()


@pytest.mark.asyncio
async def test_get_storage_stats_empty_when_no_roots(service):
    # No configured/existing library root -> zeroed stats, no DB hit.
    _, _, _, cache = service
    library_repo = AsyncMock()
    prefs = _make_preferences([])
    svc = LocalFilesService(
        library_repo=library_repo, preferences_service=prefs, cache=cache
    )

    stats = await svc.get_storage_stats()

    assert stats.total_tracks == 0
    assert stats.total_albums == 0
    library_repo.get_stats.assert_not_awaited()


def _crate_row(fid: str, **o) -> dict:
    base = dict(
        id=fid,
        track_title=f"T{fid}",
        album_title=f"Alb{fid}",
        album_artist_name=f"Art{fid}",
        artist_name=f"Art{fid}",
        release_group_mbid=f"rg{fid}",
        file_format="flac",
        year=2000,
        duration_seconds=120.0,
        cover_url=None,
    )
    base.update(o)
    return base


@pytest.mark.asyncio
async def test_get_crate_suggestions_tags_reasons_and_dedupes(service):
    svc, library_repo, music_dir, cache = service
    # 'a' repeats in the surprise pool to exercise dedup.
    pools = iter(
        [[_crate_row("a")], [_crate_row("b")], [_crate_row("a"), _crate_row("c")]]
    )
    library_repo.get_crate_tracks = AsyncMock(side_effect=lambda **kw: next(pools))

    items = await svc.get_crate_suggestions(limit=12)

    by_id = {i.track_file_id: i for i in items}
    assert sorted(by_id) == ["a", "b", "c"]
    assert by_id["a"].reason == "recent"  # first pool to claim the id wins
    assert by_id["b"].reason == "rediscover"
    assert by_id["c"].reason == "surprise"


@pytest.mark.asyncio
async def test_get_crate_suggestions_adds_same_era_pool_when_decade_given(service):
    svc, library_repo, music_dir, cache = service
    seen_kwargs = []

    async def fake(**kw):
        seen_kwargs.append(kw)
        # distinct id per pool since surprise and same_era both use order='random'
        return [_crate_row(kw["order"] + ("_era" if kw.get("decade") else ""))]

    library_repo.get_crate_tracks = AsyncMock(side_effect=fake)
    items = await svc.get_crate_suggestions(limit=12, decade=1990)

    orders = [kw["order"] for kw in seen_kwargs]
    assert orders == ["recent", "oldest", "random", "random"]
    assert any(kw.get("decade") == 1990 for kw in seen_kwargs)
    assert any(i.reason == "same_era" for i in items)


@pytest.mark.asyncio
async def test_get_decades_builds_shelves_and_filters_empty(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_decades = AsyncMock(
        return_value=[
            {"decade": 2010, "album_count": 5},
            {"decade": 0, "album_count": 3},  # junk decade, expect filtered out
            {"decade": 1990, "album_count": 0},  # empty, expect filtered out
        ]
    )
    library_repo.get_albums_page = AsyncMock(
        return_value=([_native_album(release_group_mbid="x", album_title="T")], 1)
    )

    shelves = await svc.get_decades()

    assert [s.decade for s in shelves] == [2010]
    assert shelves[0].label == "2010s"
    assert shelves[0].album_count == 5
    assert shelves[0].albums[0].musicbrainz_id == "x"


@pytest.mark.asyncio
async def test_search_returns_albums_and_tracks(service):
    svc, library_repo, music_dir, cache = service
    library_repo.get_albums_page = AsyncMock(
        return_value=(
            [_native_album(release_group_mbid="rg-x", album_title="Abbey Road")],
            1,
        )
    )
    library_repo.search_tracks = AsyncMock(
        return_value=[_crate_row("t1"), _crate_row("t2")]
    )

    result = await svc.search("abbey")

    assert [a.musicbrainz_id for a in result.albums] == ["rg-x"]
    assert [t.track_file_id for t in result.tracks] == ["t1", "t2"]
    _, album_kwargs = library_repo.get_albums_page.call_args
    assert album_kwargs["q"] == "abbey"
    library_repo.search_tracks.assert_awaited_once_with("abbey", limit=30)
