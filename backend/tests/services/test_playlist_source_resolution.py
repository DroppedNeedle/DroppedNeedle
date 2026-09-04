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
from core.exceptions import SourceResolutionError
from services.playlist_service import PlaylistService


_OWNER = SimpleNamespace(id="owner", role="user")


def _make_playlist(id="p-1") -> PlaylistRecord:
    return PlaylistRecord(
        id=id, name="Test", cover_image_path=None,
        created_at="2025-01-01T00:00:00+00:00",
        updated_at="2025-01-01T00:00:00+00:00",
        user_id="owner",
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
            album_id="mbid-abc",
            album_name="Sheet Music",
            artist_name="10cc",
            music_folder_ids=None,
        )


@pytest.mark.asyncio
async def test_selected_folder_scope_removes_unverified_stored_navidrome_source(tmp_path):
    service, repo = _make_service(tmp_path)
    repo.get_tracks.return_value = [
        _make_track(available_sources=["local", "navidrome"])
    ]
    nd = _make_nd_service(found=False, tracks=[])

    result = await service.resolve_track_sources(
        "p-1",
        requesting=_OWNER,
        nd_service=nd,
        navidrome_folder_ids=("folder-a",),
    )

    assert result == {"t-1": []}
    repo.batch_update_available_sources.assert_not_called()


@pytest.mark.asyncio
async def test_same_navidrome_source_is_revalidated_for_selected_scope(tmp_path):
    service, repo = _make_service(tmp_path)
    repo.get_track.return_value = _make_track()
    service._resolve_new_source_id = AsyncMock(
        side_effect=SourceResolutionError("outside selected folders")
    )

    with pytest.raises(SourceResolutionError, match="outside selected folders"):
        await service.update_track_source(
            "p-1",
            _OWNER,
            "t-1",
            source_type="navidrome",
            nd_service=AsyncMock(),
            navidrome_folder_ids=("folder-a",),
        )

    service._resolve_new_source_id.assert_awaited_once()
    repo.update_track_source.assert_not_called()


@pytest.mark.asyncio
async def test_passes_empty_strings_when_not_provided(tmp_path):
    service, _ = _make_service(tmp_path)
    nd = _make_nd_service()

    await service._resolve_album_sources("mbid-abc", None, None, nd)

    nd.get_album_match.assert_called_once_with(
        album_id="mbid-abc",
        album_name="",
        artist_name="",
        music_folder_ids=None,
    )


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
            album_id="mbid-abc",
            album_name="Sheet Music",
            artist_name="10cc",
            music_folder_ids=None,
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


class TestResolveLinksLibraryFiles:
    """A local match must also persist library_file_id so the compat shims can
    stream the entry (issue #181 - imported playlists showed empty in clients)."""

    @pytest.mark.asyncio
    async def test_links_unlinked_entry_on_local_match(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track(available_sources=["navidrome"])
        repo.get_tracks = MagicMock(return_value=[track])

        await service.resolve_track_sources(
            "p-1", local_service=_make_local_service(), nd_service=_make_nd_service(),
        )

        repo.batch_link_library_files.assert_called_once_with("p-1", {"t-1": "789"})

    @pytest.mark.asyncio
    async def test_existing_link_left_alone(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track(available_sources=["navidrome"])
        track.library_file_id = "already"
        repo.get_tracks = MagicMock(return_value=[track])

        await service.resolve_track_sources(
            "p-1", local_service=_make_local_service(), nd_service=_make_nd_service(),
        )

        repo.batch_link_library_files.assert_not_called()

    @pytest.mark.asyncio
    async def test_no_local_match_no_link(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_tracks = MagicMock(return_value=[_make_track()])

        await service.resolve_track_sources(
            "p-1", local_service=_make_local_service(found=False),
            nd_service=_make_nd_service(),
        )

        repo.batch_link_library_files.assert_not_called()


class TestStringTrackNumberRegression:
    """Regression tests: source resolution must work when track_number arrives as a string.

    Root cause: a source API returns trackNumber as a string (e.g., "6"). If not coerced,
    the source map gets string keys while playlist DB uses int keys, causing lookup misses.
    """

    @pytest.mark.asyncio
    async def test_resolve_sources_with_string_track_numbers(self, tmp_path):
        """resolve_track_sources discovers multi-source even when service returns string track_number."""
        service, repo = _make_service(tmp_path)
        track = _make_track(track_number=1, available_sources=["navidrome"])
        repo.get_tracks = MagicMock(return_value=[track])

        nd = _make_nd_service()
        # Local service returns string track_number (simulating pre-fix source data)
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
            "p-1", _OWNER, "t-1", source_type="local",
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
        await cache.set(
            "source_resolution:user:global:scope:all:mbid-abc",
            stale_data,
            ttl_seconds=3600,
        )

        jf, local, nd, plex = await service._resolve_album_sources(
            "mbid-abc", None, None, None,
        )

        assert isinstance(next(iter(local)), tuple)
        assert local[(1, 6)] == ("Speed Kills", "2608")
        assert local[(1, 1)] == ("Johnny", "2601")


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

        async def _slow_match(
            album_id, album_name, artist_name, music_folder_ids
        ):
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
            return ({}, {(1, 1): ("Song B", "789")}, {}, {})

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


class TestGuidAlbumIdFallback:
    """Pre-fix Jellyfin imports stored the Jellyfin album GUID as album_id, which
    the MBID-keyed matchers can never hit. Resolution re-keys the GUID to the
    album's MusicBrainz id via Jellyfin provider ids (no migration, #150)."""

    @pytest.mark.asyncio
    async def test_guid_album_id_resolves_local_source(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track(
            album_id="jf-guid-1",
            source_type="jellyfin",
            available_sources=["jellyfin"],
        )
        repo.get_tracks = MagicMock(return_value=[track])

        jf = _make_jf_service(found=False)
        jf.resolve_album_mbid = AsyncMock(return_value="mbid-abc")
        local = _make_local_service()

        result = await service.resolve_track_sources(
            "p-1", jf_service=jf, local_service=local,
        )

        local.match_album_by_mbid.assert_called_once_with("mbid-abc")
        assert sorted(result["t-1"]) == ["jellyfin", "local"]
        repo.batch_link_library_files.assert_called_once_with("p-1", {"t-1": "789"})
        updates = repo.batch_update_available_sources.call_args[0][1]
        assert sorted(updates["t-1"]) == ["jellyfin", "local"]

    @pytest.mark.asyncio
    async def test_guid_fallback_does_not_poison_mbid_cache_key(self, tmp_path):
        cache = AsyncMock()
        cache.get = AsyncMock(return_value=None)
        repo = MagicMock()
        service = PlaylistService(repo=repo, cache_dir=tmp_path, cache=cache)
        jf = _make_jf_service(found=False)
        jf.resolve_album_mbid = AsyncMock(return_value="mbid-abc")

        await service._resolve_album_sources("jf-guid-1", jf, None)

        set_key = cache.set.call_args[0][0]
        assert "jf-guid-1" in set_key
        assert "mbid-abc" not in set_key

    @pytest.mark.asyncio
    async def test_no_fallback_when_mbid_match_found(self, tmp_path):
        service, _ = _make_service(tmp_path)
        jf = _make_jf_service(found=False)
        jf.match_album_by_mbid = AsyncMock(
            return_value=SimpleNamespace(
                found=True,
                tracks=[
                    SimpleNamespace(
                        track_number=1, title="Wall Street Shuffle", jellyfin_id="jf-9"
                    )
                ],
            )
        )
        jf.resolve_album_mbid = AsyncMock(return_value="mbid-other")
        local = _make_local_service()

        await service._resolve_album_sources("mbid-abc", jf, local)

        jf.resolve_album_mbid.assert_not_called()
        local.match_album_by_mbid.assert_called_once_with("mbid-abc")


class TestSkippedBackendsNotCalled:
    """Unconfigured backends are gated on is_configured() before any call."""

    @pytest.mark.asyncio
    async def test_resolve_album_sources_skips_unconfigured_backends(self, tmp_path):
        service, _ = _make_service(tmp_path)
        jf = AsyncMock()
        jf.is_configured = MagicMock(return_value=False)
        local = AsyncMock()
        local.is_configured = MagicMock(return_value=False)
        nd = AsyncMock()
        nd.is_configured = MagicMock(return_value=False)
        plex = AsyncMock()
        plex.is_configured = MagicMock(return_value=False)

        jf_out, local_out, nd_out, plex_out = await service._resolve_album_sources(
            "mbid-abc", jf, local, nd, plex,
            album_name="Sheet Music", artist_name="10cc",
        )

        assert (jf_out, local_out, nd_out, plex_out) == ({}, {}, {}, {})
        jf.match_album_by_mbid.assert_not_called()
        local.match_album_by_mbid.assert_not_called()
        nd.get_album_match.assert_not_called()
        plex.get_album_match.assert_not_called()

    @pytest.mark.asyncio
    async def test_configured_backend_still_called_when_others_skipped(self, tmp_path):
        service, _ = _make_service(tmp_path)
        jf = AsyncMock()
        jf.is_configured = MagicMock(return_value=False)
        local = _make_local_service()
        local.is_configured = MagicMock(return_value=True)
        nd = AsyncMock()
        nd.is_configured = MagicMock(return_value=False)

        _, local_out, nd_out, _ = await service._resolve_album_sources(
            "mbid-abc", jf, local, nd,
            album_name="Sheet Music", artist_name="10cc",
        )

        jf.match_album_by_mbid.assert_not_called()
        nd.get_album_match.assert_not_called()
        local.match_album_by_mbid.assert_called_once_with("mbid-abc")
        assert local_out != {}
        assert nd_out == {}

    @pytest.mark.asyncio
    async def test_skipped_backends_record_degradation(self, tmp_path):
        from infrastructure.degradation import (
            clear_degradation_context,
            init_degradation_context,
            try_get_degradation_context,
        )

        service, _ = _make_service(tmp_path)
        jf = AsyncMock()
        jf.is_configured = MagicMock(return_value=False)
        nd = AsyncMock()
        nd.is_configured = MagicMock(return_value=False)

        init_degradation_context()
        try:
            await service._resolve_album_sources("mbid-abc", jf, None, nd)
            ctx = try_get_degradation_context()
            assert ctx is not None
            summary = ctx.degraded_summary()
            assert "jellyfin" in summary
            assert "navidrome" in summary
        finally:
            clear_degradation_context()


def _make_unknown_track(id="t-1") -> PlaylistTrackRecord:
    """A Spotify-imported row still waiting on its album (#381)."""
    track = _make_track(id=id, source_type="", available_sources=[])
    track.track_source_id = None
    return track


class TestResolvePromotesUnknownRows:
    """Empty source_type rows heal to the best resolved source (#381)."""

    @pytest.mark.asyncio
    async def test_promotes_empty_row_to_local(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_tracks = MagicMock(return_value=[_make_unknown_track()])

        result = await service.resolve_track_sources(
            "p-1", local_service=_make_local_service(),
        )

        assert result["t-1"] == ["local"]
        repo.update_track_source.assert_called_once()
        # AsyncPlaylistRepository forwards positionally:
        # (playlist_id, track_id, source_type, available_sources,
        #  track_source_id, plex_rating_key, library_file_id).
        args, _kwargs = repo.update_track_source.call_args
        assert args[:4] == ("p-1", "t-1", "local", ["local"])
        assert args[4] == "789"
        assert args[6] == "789"
        # The promotion already stores available_sources; the batch path skips it.
        repo.batch_update_available_sources.assert_not_called()

    @pytest.mark.asyncio
    async def test_does_not_clobber_existing_source(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_tracks = MagicMock(
            return_value=[_make_track(available_sources=["navidrome"])]
        )

        result = await service.resolve_track_sources(
            "p-1",
            local_service=_make_local_service(),
            nd_service=_make_nd_service(),
        )

        assert sorted(result["t-1"]) == ["local", "navidrome"]
        repo.update_track_source.assert_not_called()

    @pytest.mark.asyncio
    async def test_stale_cache_does_not_block_local_healing(self, tmp_path):
        repo = MagicMock()
        repo.get_playlist = MagicMock(return_value=_make_playlist())
        repo.get_tracks = MagicMock(return_value=[_make_unknown_track()])
        repo.batch_update_available_sources = MagicMock(return_value=0)
        repo.batch_link_library_files = MagicMock(return_value=0)
        cache = AsyncMock()
        # Entry cached before the download: navidrome only, no local tracks.
        cache.get = AsyncMock(
            return_value=({}, {}, {(1, 1): ("Wall Street Shuffle", "nd-456")}, {})
        )
        cache.set = AsyncMock()
        service = PlaylistService(repo=repo, cache_dir=tmp_path, cache=cache)

        result = await service.resolve_track_sources(
            "p-1",
            local_service=_make_local_service(),
            nd_service=_make_nd_service(),
        )

        assert "local" in result["t-1"]
        repo.update_track_source.assert_called_once()
        args, _kwargs = repo.update_track_source.call_args
        assert args[:4] == ("p-1", "t-1", "local", result["t-1"])
        assert args[4] == "789"
        assert args[6] == "789"

    @pytest.mark.asyncio
    async def test_scoped_promotion_to_local_strips_navidrome(self, tmp_path):
        """A scoped resolve healing to local must not persist navidrome globally."""
        service, repo = _make_service(tmp_path)
        repo.get_tracks = MagicMock(return_value=[_make_unknown_track()])

        result = await service.resolve_track_sources(
            "p-1",
            local_service=_make_local_service(),
            nd_service=_make_nd_service(),
            navidrome_folder_ids=("folder-a",),
        )

        assert "local" in result["t-1"]
        repo.update_track_source.assert_called_once()
        args, _kwargs = repo.update_track_source.call_args
        assert args[0] == "p-1"
        assert args[1] == "t-1"
        assert args[2] == "local"
        persisted = args[3]
        assert "navidrome" not in persisted
        assert "local" in persisted
        assert args[4] == "789"
        assert args[6] == "789"
        repo.batch_update_available_sources.assert_not_called()
