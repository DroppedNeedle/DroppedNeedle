import pytest
from unittest.mock import AsyncMock, MagicMock, patch
from pathlib import Path
from types import SimpleNamespace

from core.exceptions import InvalidPlaylistDataError, PlaylistNotFoundError, SourceResolutionError
from repositories.playlist_repository import PlaylistRecord, PlaylistTrackRecord
from services.playlist_service import PlaylistService


# Mutating service methods now take a requesting user and enforce ownership; these
# unit tests drive the owner path (record owner == requester) so they exercise the
# core logic. Ownership/redaction is covered by tests/routes/test_playlist_ownership.py.
_OWNER = SimpleNamespace(id="owner", role="user")


def _make_playlist(id="p-1", name="Test", cover_image_path=None, user_id="owner") -> PlaylistRecord:
    return PlaylistRecord(
        id=id, name=name, cover_image_path=cover_image_path,
        created_at="2025-01-01T00:00:00+00:00",
        updated_at="2025-01-01T00:00:00+00:00",
        user_id=user_id,
    )


def _make_track(id="t-1", playlist_id="p-1", position=0) -> PlaylistTrackRecord:
    return PlaylistTrackRecord(
        id=id, playlist_id=playlist_id, position=position,
        track_name="Track", artist_name="Artist", album_name="Album",
        album_id=None, artist_id=None, track_source_id=None, cover_url=None,
        source_type="local", available_sources=None, format=None,
        track_number=None, disc_number=None, duration=None,
        created_at="2025-01-01T00:00:00+00:00",
    )


def _make_service(tmp_path: Path) -> tuple[PlaylistService, MagicMock]:
    repo = MagicMock()
    repo.create_playlist = MagicMock(return_value=_make_playlist())
    repo.get_playlist = MagicMock(return_value=_make_playlist())
    repo.get_all_playlists = MagicMock(return_value=[])
    repo.update_playlist = MagicMock(return_value=_make_playlist())
    repo.delete_playlist = MagicMock(return_value=True)
    repo.add_tracks = MagicMock(return_value=[_make_track()])
    repo.remove_track = MagicMock(return_value=True)
    repo.reorder_track = MagicMock(return_value=2)
    repo.update_track_source = MagicMock(return_value=_make_track())
    repo.get_tracks = MagicMock(return_value=[])
    service = PlaylistService(repo=repo, cache_dir=tmp_path)
    return service, repo


class TestCreatePlaylist:
    @pytest.mark.asyncio
    async def test_valid_name(self, tmp_path):
        service, repo = _make_service(tmp_path)
        result = await service.create_playlist("My Playlist", user_id="owner")
        assert result.name == "Test"
        repo.create_playlist.assert_called_once_with("My Playlist", None, "owner")

    @pytest.mark.asyncio
    async def test_empty_name(self, tmp_path):
        service, _ = _make_service(tmp_path)
        with pytest.raises(InvalidPlaylistDataError):
            await service.create_playlist("", user_id="owner")

    @pytest.mark.asyncio
    async def test_whitespace_name(self, tmp_path):
        service, _ = _make_service(tmp_path)
        with pytest.raises(InvalidPlaylistDataError):
            await service.create_playlist("   ", user_id="owner")

    @pytest.mark.asyncio
    async def test_strips_whitespace(self, tmp_path):
        service, repo = _make_service(tmp_path)
        await service.create_playlist("  Hello  ", user_id="owner")
        repo.create_playlist.assert_called_once_with("Hello", None, "owner")


class TestGetPlaylist:
    @pytest.mark.asyncio
    async def test_existing(self, tmp_path):
        service, _ = _make_service(tmp_path)
        result = await service.get_playlist("p-1")
        assert result.id == "p-1"

    @pytest.mark.asyncio
    async def test_not_found(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_playlist = MagicMock(return_value=None)
        with pytest.raises(PlaylistNotFoundError):
            await service.get_playlist("nonexistent")


class TestUpdatePlaylist:
    @pytest.mark.asyncio
    async def test_valid_update(self, tmp_path):
        service, repo = _make_service(tmp_path)
        result = await service.update_playlist("p-1", _OWNER, name="New")
        assert result is not None
        repo.update_playlist.assert_called_once()

    @pytest.mark.asyncio
    async def test_not_found(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.update_playlist = MagicMock(return_value=None)
        with pytest.raises(PlaylistNotFoundError):
            await service.update_playlist("nonexistent", _OWNER, name="X")

    @pytest.mark.asyncio
    async def test_empty_name(self, tmp_path):
        service, _ = _make_service(tmp_path)
        with pytest.raises(InvalidPlaylistDataError):
            await service.update_playlist("p-1", _OWNER, name="")


class TestDeletePlaylist:
    @pytest.mark.asyncio
    async def test_successful(self, tmp_path):
        service, repo = _make_service(tmp_path)
        await service.delete_playlist("p-1", _OWNER)
        repo.delete_playlist.assert_called_once_with("p-1")

    @pytest.mark.asyncio
    async def test_not_found(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.delete_playlist = MagicMock(return_value=False)
        with pytest.raises(PlaylistNotFoundError):
            await service.delete_playlist("nonexistent", _OWNER)


class TestAddTracks:
    @pytest.mark.asyncio
    async def test_valid(self, tmp_path):
        service, repo = _make_service(tmp_path)
        tracks = [{"track_name": "T", "artist_name": "A", "album_name": "AL", "source_type": "local"}]
        result = await service.add_tracks("p-1", _OWNER, tracks)
        assert len(result) == 1

    @pytest.mark.asyncio
    async def test_empty_list(self, tmp_path):
        service, _ = _make_service(tmp_path)
        with pytest.raises(InvalidPlaylistDataError):
            await service.add_tracks("p-1", _OWNER, [])

    @pytest.mark.asyncio
    async def test_playlist_not_found(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_playlist = MagicMock(return_value=None)
        with pytest.raises(PlaylistNotFoundError):
            await service.add_tracks("nonexistent", _OWNER, [{"track_name": "T", "artist_name": "A", "album_name": "AL", "source_type": "local"}])

    @pytest.mark.asyncio
    async def test_local_track_source_id_links_library_file(self, tmp_path):
        # web-UI adds send the library file id as track_source_id; without the
        # auto-link the compat shims treat the entry as unstreamable (issue #181)
        service, repo = _make_service(tmp_path)
        await service.add_tracks("p-1", _OWNER, [
            {"track_name": "T", "artist_name": "A", "album_name": "AL",
             "source_type": "local", "track_source_id": "file-1"},
            {"track_name": "T2", "artist_name": "A", "album_name": "AL",
             "source_type": "howler", "track_source_id": "file-2"},  # alias of local
            {"track_name": "T3", "artist_name": "A", "album_name": "AL",
             "source_type": "navidrome", "track_source_id": "nd-3"},
        ])
        sent = repo.add_tracks.call_args[0][1]
        assert sent[0]["library_file_id"] == "file-1"
        assert sent[1]["library_file_id"] == "file-2"
        assert "library_file_id" not in sent[2]

    @pytest.mark.asyncio
    async def test_explicit_library_file_id_wins(self, tmp_path):
        service, repo = _make_service(tmp_path)
        await service.add_tracks("p-1", _OWNER, [
            {"track_name": "T", "artist_name": "A", "album_name": "AL",
             "source_type": "local", "track_source_id": "file-1",
             "library_file_id": "explicit"},
        ])
        sent = repo.add_tracks.call_args[0][1]
        assert sent[0]["library_file_id"] == "explicit"


class TestRemoveTrack:
    @pytest.mark.asyncio
    async def test_successful(self, tmp_path):
        service, repo = _make_service(tmp_path)
        await service.remove_track("p-1", _OWNER, "t-1")
        repo.remove_track.assert_called_once_with("p-1", "t-1")

    @pytest.mark.asyncio
    async def test_not_found(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.remove_track = MagicMock(return_value=False)
        with pytest.raises(PlaylistNotFoundError):
            await service.remove_track("p-1", _OWNER, "nonexistent")


class TestReorderTrack:
    @pytest.mark.asyncio
    async def test_valid(self, tmp_path):
        service, repo = _make_service(tmp_path)
        result = await service.reorder_track("p-1", _OWNER, "t-1", 2)
        assert result == 2
        repo.reorder_track.assert_called_once_with("p-1", "t-1", 2)

    @pytest.mark.asyncio
    async def test_negative_position(self, tmp_path):
        service, _ = _make_service(tmp_path)
        with pytest.raises(InvalidPlaylistDataError):
            await service.reorder_track("p-1", _OWNER, "t-1", -1)

    @pytest.mark.asyncio
    async def test_not_found(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.reorder_track = MagicMock(return_value=None)
        with pytest.raises(PlaylistNotFoundError):
            await service.reorder_track("p-1", _OWNER, "nonexistent", 0)


class TestUploadCover:
    @pytest.mark.asyncio
    async def test_invalid_mime(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_playlist = MagicMock(return_value=_make_playlist(id="abcdef-1234"))
        with pytest.raises(InvalidPlaylistDataError, match="Invalid image type"):
            await service.upload_cover("abcdef-1234", _OWNER, b"data", "application/pdf")

    @pytest.mark.asyncio
    async def test_too_large(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_playlist = MagicMock(return_value=_make_playlist(id="abcdef-1234"))
        data = b"x" * (2 * 1024 * 1024 + 1)
        with pytest.raises(InvalidPlaylistDataError, match="too large"):
            await service.upload_cover("abcdef-1234", _OWNER, data, "image/png")

    @pytest.mark.asyncio
    async def test_path_traversal_id(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_playlist = MagicMock(return_value=_make_playlist(id="../evil"))
        with pytest.raises(InvalidPlaylistDataError, match="Invalid playlist ID"):
            await service.upload_cover("../evil", _OWNER, b"data", "image/png")

    @pytest.mark.asyncio
    async def test_valid_upload(self, tmp_path):
        service, repo = _make_service(tmp_path)
        playlist = _make_playlist(id="abcdef-1234")
        repo.get_playlist = MagicMock(return_value=playlist)

        result = await service.upload_cover("abcdef-1234", _OWNER, b"PNG_DATA", "image/png")
        assert result == "/api/v1/playlists/abcdef-1234/cover"
        repo.update_playlist.assert_called()

        cover_dir = tmp_path / "covers" / "playlists"
        assert (cover_dir / "abcdef-1234.png").exists()

    @pytest.mark.asyncio
    async def test_replaces_old_cover(self, tmp_path):
        service, repo = _make_service(tmp_path)
        playlist = _make_playlist(id="abcdef-1234")
        repo.get_playlist = MagicMock(return_value=playlist)

        await service.upload_cover("abcdef-1234", _OWNER, b"OLD_PNG", "image/png")
        cover_dir = tmp_path / "covers" / "playlists"
        assert (cover_dir / "abcdef-1234.png").exists()

        await service.upload_cover("abcdef-1234", _OWNER, b"NEW_JPEG", "image/jpeg")
        assert not (cover_dir / "abcdef-1234.png").exists()
        assert (cover_dir / "abcdef-1234.jpg").exists()
        assert (cover_dir / "abcdef-1234.jpg").read_bytes() == b"NEW_JPEG"


class TestRemoveCover:
    @pytest.mark.asyncio
    async def test_removes_file_and_clears_path(self, tmp_path):
        cover_dir = tmp_path / "covers" / "playlists"
        cover_dir.mkdir(parents=True)
        cover_file = cover_dir / "p-1.png"
        cover_file.write_bytes(b"img")

        service, repo = _make_service(tmp_path)
        repo.get_playlist = MagicMock(
            return_value=_make_playlist(cover_image_path=str(cover_file)),
        )

        await service.remove_cover("p-1", _OWNER)
        assert not cover_file.exists()
        repo.update_playlist.assert_called()

    @pytest.mark.asyncio
    async def test_stale_cover_path_succeeds(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_playlist = MagicMock(
            return_value=_make_playlist(cover_image_path="/nonexistent/stale.png"),
        )
        await service.remove_cover("p-1", _OWNER)
        repo.update_playlist.assert_called()


class TestSourceTypeValidation:
    @pytest.mark.asyncio
    async def test_invalid_source_type_in_add_tracks(self, tmp_path):
        service, _ = _make_service(tmp_path)
        tracks = [{"track_name": "T", "artist_name": "A", "album_name": "AL", "source_type": "invalid"}]
        with pytest.raises(InvalidPlaylistDataError, match="Invalid source_type"):
            await service.add_tracks("p-1", _OWNER, tracks)

    @pytest.mark.asyncio
    async def test_valid_source_types_in_add_tracks(self, tmp_path):
        service, repo = _make_service(tmp_path)
        for st in ("local", "jellyfin", "youtube", ""):
            tracks = [{"track_name": "T", "artist_name": "A", "album_name": "AL", "source_type": st}]
            result = await service.add_tracks("p-1", _OWNER, tracks)
            assert len(result) == 1

    @pytest.mark.asyncio
    async def test_invalid_source_type_in_update_track(self, tmp_path):
        service, _ = _make_service(tmp_path)
        with pytest.raises(InvalidPlaylistDataError, match="Invalid source_type"):
            await service.update_track_source("p-1", _OWNER, "t-1", source_type="bogus")


class TestUpdatePlaylistWithDetail:
    @pytest.mark.asyncio
    async def test_returns_playlist_and_tracks(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_tracks = MagicMock(return_value=[_make_track()])
        playlist, tracks = await service.update_playlist_with_detail("p-1", _OWNER, name="New")
        assert playlist is not None
        assert len(tracks) == 1
        repo.update_playlist.assert_called_once()


class TestCheckTrackMembership:
    @pytest.mark.asyncio
    async def test_delegates_to_repo(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.check_track_membership = MagicMock(return_value={"p-1": [0]})
        result = await service.check_track_membership([
            ("Song", "Artist", "Album"),
        ])
        assert result == {"p-1": [0]}
        repo.check_track_membership.assert_called_once_with([
            ("Song", "Artist", "Album"),
        ], None)


def _jf_match(tracks):
    return SimpleNamespace(
        found=True,
        tracks=[SimpleNamespace(title=t[0], track_number=t[1], jellyfin_id=t[2]) for t in tracks],
    )


def _local_match(tracks):
    return SimpleNamespace(
        found=True,
        tracks=[SimpleNamespace(title=t[0], track_number=t[1], track_file_id=t[2]) for t in tracks],
    )


def _make_track_with_album(id="t-1", track_name="Track", track_number=1, album_id="mb-album-1", source_type="local"):
    return PlaylistTrackRecord(
        id=id, playlist_id="p-1", position=0,
        track_name=track_name, artist_name="Artist", album_name="Album",
        album_id=album_id, artist_id=None, track_source_id="old-src-id",
        cover_url=None, source_type=source_type, available_sources=None,
        format=None, track_number=track_number, disc_number=None, duration=None,
        created_at="2025-01-01T00:00:00+00:00",
    )


class TestResolveTrackSources:
    @pytest.mark.asyncio
    async def test_resolves_jellyfin_and_local(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track_with_album(id="t-1", track_name="Song One", track_number=1)
        repo.get_tracks = MagicMock(return_value=[track])

        jf_svc = AsyncMock()
        jf_svc.match_album_by_mbid.return_value = _jf_match([("Song One", 1, "jf-id-1")])
        local_svc = AsyncMock()
        local_svc.match_album_by_mbid.return_value = _local_match([("Song One", 1, 42)])

        result = await service.resolve_track_sources("p-1", jf_service=jf_svc, local_service=local_svc)
        assert "t-1" in result
        assert "jellyfin" in result["t-1"]
        assert "local" in result["t-1"]

    @pytest.mark.asyncio
    async def test_no_album_id_returns_current_source(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track(id="t-1")
        track = PlaylistTrackRecord(
            id="t-1", playlist_id="p-1", position=0,
            track_name="Song", artist_name="Artist", album_name="Album",
            album_id=None, artist_id=None, track_source_id=None,
            cover_url=None, source_type="youtube", available_sources=None,
            format=None, track_number=None, disc_number=None, duration=None,
            created_at="2025-01-01T00:00:00+00:00",
        )
        repo.get_tracks = MagicMock(return_value=[track])

        result = await service.resolve_track_sources("p-1", jf_service=AsyncMock(), local_service=AsyncMock())
        assert result["t-1"] == ["youtube"]

    @pytest.mark.asyncio
    async def test_empty_playlist_returns_empty(self, tmp_path):
        service, repo = _make_service(tmp_path)
        repo.get_tracks = MagicMock(return_value=[])
        result = await service.resolve_track_sources("p-1")
        assert result == {}

    @pytest.mark.asyncio
    async def test_service_error_skips_source(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track_with_album()
        repo.get_tracks = MagicMock(return_value=[track])

        jf_svc = AsyncMock()
        jf_svc.match_album_by_mbid.side_effect = RuntimeError("connection failed")
        local_svc = AsyncMock()
        local_svc.match_album_by_mbid.return_value = SimpleNamespace(found=False)

        result = await service.resolve_track_sources("p-1", jf_service=jf_svc, local_service=local_svc)
        assert "t-1" in result
        assert "jellyfin" not in result["t-1"]


class TestResolveNewSourceId:
    @pytest.mark.asyncio
    async def test_switch_to_jellyfin(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track_with_album(track_name="Track Title", source_type="local")
        repo.get_track = MagicMock(return_value=track)
        repo.update_track_source = MagicMock(return_value=PlaylistTrackRecord(
            id="t-1", playlist_id="p-1", position=0,
            track_name="Track Title", artist_name="Artist", album_name="Album",
            album_id="mb-album-1", artist_id=None, track_source_id="jf-id-1",
            cover_url=None, source_type="jellyfin", available_sources=None,
            format=None, track_number=1, disc_number=None, duration=None,
            created_at="2025-01-01T00:00:00+00:00",
        ))

        jf_svc = AsyncMock()
        jf_svc.match_album_by_mbid.return_value = _jf_match([("Track Title", 1, "jf-id-1")])
        local_svc = AsyncMock()

        result = await service.update_track_source(
            "p-1", _OWNER, "t-1", source_type="jellyfin",
            jf_service=jf_svc, local_service=local_svc,
        )
        assert result.track_source_id == "jf-id-1"

    @pytest.mark.asyncio
    async def test_no_album_id_raises(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = PlaylistTrackRecord(
            id="t-1", playlist_id="p-1", position=0,
            track_name="Song", artist_name="Artist", album_name="Album",
            album_id=None, artist_id=None, track_source_id=None,
            cover_url=None, source_type="local", available_sources=None,
            format=None, track_number=None, disc_number=None, duration=None,
            created_at="2025-01-01T00:00:00+00:00",
        )
        repo.get_track = MagicMock(return_value=track)

        with pytest.raises(SourceResolutionError, match="missing album_id"):
            await service.update_track_source(
                "p-1", _OWNER, "t-1", source_type="jellyfin",
                jf_service=AsyncMock(), local_service=AsyncMock(),
            )

    @pytest.mark.asyncio
    async def test_track_not_found_in_source_raises(self, tmp_path):
        service, repo = _make_service(tmp_path)
        track = _make_track_with_album(track_name="My Song", source_type="local")
        repo.get_track = MagicMock(return_value=track)

        jf_svc = AsyncMock()
        jf_svc.match_album_by_mbid.return_value = SimpleNamespace(found=False)
        local_svc = AsyncMock()

        with pytest.raises(SourceResolutionError, match="not found in Jellyfin"):
            await service.update_track_source(
                "p-1", _OWNER, "t-1", source_type="jellyfin",
                jf_service=jf_svc, local_service=local_svc,
            )
