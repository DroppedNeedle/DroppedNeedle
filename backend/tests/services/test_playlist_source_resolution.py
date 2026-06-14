"""Tests for playlist source resolution fixes.

Covers:
- _resolve_album_sources passes album_name/artist_name to Navidrome
- resolve_track_sources persists resolved sources to DB (superset guard)
- resolve_track_sources correctly discovers multi-source tracks
- resolve_track_sources resolves album groups concurrently (large-playlist hang)
"""
import asyncio
import pytest
from unittest.mock import AsyncMock, MagicMock
from pathlib import Path
from types import SimpleNamespace

from repositories.playlist_repository import PlaylistRecord, PlaylistTrackRecord
from services.playlist_service import PlaylistService


def _make_playlist(id="p-1") -> PlaylistRecord:
    return PlaylistRecord(
        id=id, name="Test", cover_image_path=None,
        created_at="2025-01-01T00:00:00+00:00",
        updated_at="2025-01-01T00:00:00+00:00",
    )


def _make_track(
    id="t-1", album_id="mbid-abc", track_number=1,
    track_name="Wall Street Shuffle", artist_name="10cc", album_name="Sheet Music",
    source_type="navidrome", available_sources=None,
) -> PlaylistTrackRecord:
    return PlaylistTrackRecord(
        id=id, playlist_id="p-1", position=0,
        track_name=track_name, artist_name=artist_name, album_name=album_name,
        album_id=album_id, artist_id=None, track_source_id="nd-123",
        cover_url=None, source_type=source_type,
        available_sources=available_sources,
        format="flac", track_number=track_number, disc_number=None, duration=240,
        created_at="2025-01-01T00:00:00+00:00",
    )


def _make_service(tmp_path: Path) -> tuple[PlaylistService, MagicMock]:
    repo = MagicMock()
    repo.get_playlist = MagicMock(return_value=_make_playlist())
    repo.get_tracks = MagicMock(return_value=[])
    repo.batch_update_available_sources = MagicMock(return_value=0)
    service = PlaylistService(repo=repo, cache_dir=tmp_path)
    return service, repo


def _make_nd_service(found=True, tracks=None):
    nd = AsyncMock()
    if tracks is None:
        tracks = [SimpleNamespace(track_number=1, title="Wall Street Shuffle", navidrome_id="nd-456")]
    nd.get_album_match = AsyncMock(return_value=SimpleNamespace(found=found, tracks=tracks))
    return nd


def _make_local_service(found=True, tracks=None):
    local = AsyncMock()
    if tracks is None:
        tracks = [SimpleNamespace(track_number=1, title="Wall Street Shuffle", track_file_id=789)]
    local.match_album_by_mbid = AsyncMock(return_value=SimpleNamespace(found=found, tracks=tracks))
    return local


def _make_jf_service(found=False):
    jf = AsyncMock()
    jf.match_album_by_mbid = AsyncMock(return_value=SimpleNamespace(found=found, tracks=[]))
    return jf


class TestResolveAlbumSourcesPassesMetadata:
    """Verify _resolve_album_sources passes album_name/artist_name to Navidrome."""

    @pytest.mark.asyncio
    async def test_passes_album_name_and_artist_name_to_navidrome(self, tmp_path):
        service, _ = _make_service(tmp_path)
        nd = _make_nd_service()

        await service._resolve_album_sources(
            "mbid-abc", None, None, nd,
            album_name="Sheet Music", artist_name="10cc",
        )

        nd.get_album_match.assert_called_once_with(
            album_id="mbid-abc", album_name="Sheet Music", artist_name="10cc",
        )
        assert True

    @pytest.mark.asyncio
    async def test_passes_empty_strings_when_not_provided(self, tmp_path):
        service, _ = _make_service(tmp_path)
        nd = _make_nd_service()

        await service._resolve_album_sources("mbid-abc", None, None, nd)

        nd.get_album_match.assert_called_once_with(
            album_id="mbid-abc", album_name="", artist_name="",
        )
        assert True


class TestResolveTrackSourcesDiscovery:
    """Verify resolve_track_sources correctly discovers multi-source tracks."""

    @pytest.mark.asyncio
    async def test_discovers_local_and_navidrome_sources(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track(available_sources=["navidrome"])
        repo.get_tracks = MagicMock(return_value=[track])

        nd = _make_nd_service()
        local = _make_local_service()
        jf = _make_jf_service()

        result = await service.resolve_track_sources(
            "p-1", jf_service=jf, local_service=local, nd_service=nd,
        )

        assert "t-1" in result
        assert sorted(result["t-1"]) == ["local", "navidrome"]

    @pytest.mark.asyncio
    async def test_extracts_album_metadata_from_tracks(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track()
        repo.get_tracks = MagicMock(return_value=[track])

        nd = _make_nd_service()

        await service.resolve_track_sources("p-1", nd_service=nd)

        nd.get_album_match.assert_called_once_with(
            album_id="mbid-abc", album_name="Sheet Music", artist_name="10cc",
        )
        assert True

    @pytest.mark.asyncio
    async def test_no_album_tracks_keep_single_source(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track(album_id=None, track_number=None)
        repo.get_tracks = MagicMock(return_value=[track])

        result = await service.resolve_track_sources("p-1")

        assert result["t-1"] == ["navidrome"]


class TestResolveTrackSourcesPersistence:
    """Verify resolve_track_sources persists resolved sources (superset guard)."""

    @pytest.mark.asyncio
    async def test_persists_when_resolved_is_superset(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track(available_sources=["navidrome"])
        repo.get_tracks = MagicMock(return_value=[track])

        nd = _make_nd_service()
        local = _make_local_service()

        await service.resolve_track_sources("p-1", local_service=local, nd_service=nd)

        repo.batch_update_available_sources.assert_called_once()
        call_args = repo.batch_update_available_sources.call_args
        assert call_args[0][0] == "p-1"
        updates = call_args[0][1]
        assert "t-1" in updates
        assert sorted(updates["t-1"]) == ["local", "navidrome"]

    @pytest.mark.asyncio
    async def test_skips_persist_when_no_change(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track(available_sources=["local", "navidrome"])
        repo.get_tracks = MagicMock(return_value=[track])

        nd = _make_nd_service()
        local = _make_local_service()

        await service.resolve_track_sources("p-1", local_service=local, nd_service=nd)

        repo.batch_update_available_sources.assert_not_called()

    @pytest.mark.asyncio
    async def test_skips_persist_when_resolved_is_subset(self, tmp_path):
        """Superset guard: don't overwrite if resolution lost a source (e.g. service down)."""
        service, repo = _make_service(tmp_path)
        track = _make_track(available_sources=["jellyfin", "local", "navidrome"])
        repo.get_tracks = MagicMock(return_value=[track])

        nd = _make_nd_service()
        local = _make_local_service()

        await service.resolve_track_sources("p-1", local_service=local, nd_service=nd)

        repo.batch_update_available_sources.assert_not_called()


class TestStringTrackNumberRegression:
    """Regression tests: source resolution must work when track_number arrives as a string.

    Root cause: Lidarr API returns trackNumber as a string (e.g., "6"). If not coerced,
    the source map gets string keys while playlist DB uses int keys, causing lookup misses.
    """

    @pytest.mark.asyncio
    async def test_resolve_sources_with_string_track_numbers(self, tmp_path):
        """resolve_track_sources discovers multi-source even when service returns string track_number."""
        service, repo = _make_service(tmp_path)
        track = _make_track(track_number=1, available_sources=["navidrome"])
        repo.get_tracks = MagicMock(return_value=[track])

        nd = _make_nd_service()
        # Local service returns string track_number (simulating pre-fix Lidarr data)
        local = _make_local_service(tracks=[
            SimpleNamespace(track_number="1", title="Wall Street Shuffle", track_file_id=789),
        ])
        jf = _make_jf_service()

        result = await service.resolve_track_sources(
            "p-1", jf_service=jf, local_service=local, nd_service=nd,
        )

        assert "t-1" in result
        assert sorted(result["t-1"]) == ["local", "navidrome"]

    @pytest.mark.asyncio
    async def test_update_track_source_with_string_track_numbers(self, tmp_path):
        """update_track_source resolves local source_id even when track_number is a string."""
        service, repo = _make_service(tmp_path)
        track = _make_track(
            track_number=6, source_type="navidrome",
            track_name="Speed Kills", available_sources=["local", "navidrome"],
        )
        repo.get_track = MagicMock(return_value=track)
        repo.update_track_source = MagicMock(return_value=track)

        local = _make_local_service(tracks=[
            SimpleNamespace(track_number="6", title="Speed Kills", track_file_id=2608),
        ])
        nd = _make_nd_service(tracks=[
            SimpleNamespace(track_number=6, title="Speed Kills", navidrome_id="nd-456"),
        ])

        await service.update_track_source(
            "p-1", "t-1", source_type="local",
            jf_service=None, local_service=local, nd_service=nd,
        )

        repo.update_track_source.assert_called_once()
        call_args = repo.update_track_source.call_args
        # positional: (playlist_id, track_id, source_type, available_sources, track_source_id)
        assert call_args[0][2] == "local"
        assert call_args[0][4] == "2608"

    @pytest.mark.asyncio
    async def test_cached_string_keys_are_normalized_on_read(self, tmp_path):
        """Stale cached source maps with string keys are normalized to int keys."""
        service, repo = _make_service(tmp_path)

        from infrastructure.cache.memory_cache import InMemoryCache
        cache = InMemoryCache()
        service._cache = cache
        stale_data = (
            {},
            {"6": ("Speed Kills", "2608"), "1": ("Johnny", "2601")},
            {6: ("Speed Kills", "nd-456"), 1: ("Johnny", "nd-401")},
        )
        await cache.set("source_resolution:mbid-abc", stale_data, ttl_seconds=3600)

        jf, local, nd, plex = await service._resolve_album_sources(
            "mbid-abc", None, None, None,
        )

        assert isinstance(next(iter(local)), int)
        assert local[6] == ("Speed Kills", "2608")
        assert local[1] == ("Johnny", "2601")


class TestResolveTrackSourcesConcurrency:
    """Large playlists must resolve album groups concurrently, not one-by-one.

    A 300+ track playlist spans hundreds of albums. Resolving them sequentially
    turned the synchronous /resolve-sources call into hundreds of serial external
    round-trips, so big playlists appeared to hang (GH #63 / MUS-18). These tests
    would fail against the old sequential loop.
    """

    @pytest.mark.asyncio
    async def test_album_groups_resolved_concurrently(self, tmp_path):
        service, repo = _make_service(tmp_path)
        tracks = [
            _make_track(id=f"t-{i}", album_id=f"mbid-{i}", track_number=1)
            for i in range(5)
        ]
        repo.get_tracks = MagicMock(return_value=tracks)

        in_flight = 0
        max_in_flight = 0

        async def _slow_match(album_id, album_name, artist_name):
            nonlocal in_flight, max_in_flight
            in_flight += 1
            max_in_flight = max(max_in_flight, in_flight)
            # Yield repeatedly so sibling resolutions can interleave; under the
            # old sequential loop each call finishes before the next begins.
            for _ in range(3):
                await asyncio.sleep(0)
            in_flight -= 1
            return SimpleNamespace(
                found=True,
                tracks=[SimpleNamespace(track_number=1, title="Wall Street Shuffle", navidrome_id="nd")],
            )

        nd = AsyncMock()
        nd.get_album_match = AsyncMock(side_effect=_slow_match)

        result = await service.resolve_track_sources("p-1", nd_service=nd)

        assert nd.get_album_match.await_count == 5
        assert max_in_flight >= 2  # proves concurrent resolution, not serial
        assert set(result.keys()) == {f"t-{i}" for i in range(5)}

    @pytest.mark.asyncio
    async def test_one_album_failure_does_not_abort_resolution(self, tmp_path):
        """A single album group raising must not discard the other groups' results."""
        service, repo = _make_service(tmp_path)
        tracks = [
            _make_track(id="t-a", album_id="mbid-a", track_number=1, track_name="Song A"),
            _make_track(id="t-b", album_id="mbid-b", track_number=1, track_name="Song B"),
        ]
        repo.get_tracks = MagicMock(return_value=tracks)

        async def _flaky(album_id, *args, **kwargs):
            if album_id == "mbid-a":
                raise RuntimeError("boom")
            # (jf_by_num, local_by_num, nd_by_num, plex_by_num)
            return ({}, {1: ("Song B", "789")}, {}, {})

        service._resolve_album_sources = AsyncMock(side_effect=_flaky)

        result = await service.resolve_track_sources(
            "p-1", local_service=AsyncMock(), nd_service=AsyncMock(),
        )

        # Failed album degrades to its stored source_type; healthy album still enriches.
        assert result["t-a"] == ["navidrome"]
        assert sorted(result["t-b"]) == ["local", "navidrome"]

    @pytest.mark.asyncio
    async def test_concurrent_resolution_preserves_per_track_mapping(self, tmp_path):
        """Each album's resolved tracks must still map back to the right playlist track."""
        service, repo = _make_service(tmp_path)
        tracks = [
            _make_track(
                id="t-a", album_id="mbid-a", track_number=1,
                track_name="Song A", source_type="navidrome",
            ),
            _make_track(
                id="t-b", album_id="mbid-b", track_number=1,
                track_name="Song B", source_type="navidrome",
            ),
        ]
        repo.get_tracks = MagicMock(return_value=tracks)

        async def _match(album_id, album_name, artist_name):
            title = "Song A" if album_id == "mbid-a" else "Song B"
            return SimpleNamespace(
                found=True,
                tracks=[SimpleNamespace(track_number=1, title=title, navidrome_id="nd")],
            )

        nd = AsyncMock()
        nd.get_album_match = AsyncMock(side_effect=_match)

        result = await service.resolve_track_sources("p-1", nd_service=nd)

        assert sorted(result["t-a"]) == ["navidrome"]
        assert sorted(result["t-b"]) == ["navidrome"]
