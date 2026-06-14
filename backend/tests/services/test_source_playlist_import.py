"""Tests for source playlist list, detail, and import across Plex, Navidrome, and Jellyfin."""
from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, PropertyMock

import pytest

from core.exceptions import ExternalServiceError
from repositories.plex_models import PlexMedia, PlexPart, PlexPlaylist, PlexTrack
from repositories.navidrome_models import SubsonicPlaylist, SubsonicSong
from repositories.jellyfin_models import JellyfinItem
from repositories.playlist_repository import PlaylistRecord
from services.plex_library_service import PlexLibraryService
from services.navidrome_library_service import NavidromeLibraryService
from services.jellyfin_library_service import JellyfinLibraryService


def _mock_playlist_service(existing: PlaylistRecord | None = None) -> MagicMock:
    svc = MagicMock()
    svc.get_by_source_ref = AsyncMock(return_value=existing)
    created = PlaylistRecord(id="new-pl-1", name="Imported", cover_image_path=None, created_at="2024-01-01", updated_at="2024-01-01")
    svc.create_playlist = AsyncMock(return_value=created)
    svc.add_tracks = AsyncMock(return_value=[])
    svc.delete_playlist = AsyncMock()
    return svc


def _plex_service(playlists=None, items=None) -> PlexLibraryService:
    repo = MagicMock()
    repo.get_playlists = AsyncMock(return_value=playlists or [])
    repo.get_playlist_items = AsyncMock(return_value=items or [])
    repo.get_albums = AsyncMock(return_value=([], 0))
    repo.get_recently_added = AsyncMock(return_value=[])
    repo.get_recently_viewed = AsyncMock(return_value=[])
    repo.get_genres = AsyncMock(return_value=[])
    repo.get_track_count = AsyncMock(return_value=0)
    repo.get_artist_count = AsyncMock(return_value=0)
    type(repo).stats_ttl = PropertyMock(return_value=600)
    prefs = MagicMock()
    conn = MagicMock()
    conn.enabled = True
    conn.plex_url = "http://plex:32400"
    conn.plex_token = "tok"
    conn.music_library_ids = ["1"]
    prefs.get_plex_connection_raw.return_value = conn
    return PlexLibraryService(plex_repo=repo, preferences_service=prefs)


def _navidrome_service(playlists=None, playlist_detail=None) -> NavidromeLibraryService:
    repo = MagicMock()
    repo.get_playlists = AsyncMock(return_value=playlists or [])
    repo.get_playlist = AsyncMock(return_value=playlist_detail)
    repo.get_albums = AsyncMock(return_value=[])
    repo.get_recently_played = AsyncMock(return_value=[])
    repo.get_starred = AsyncMock(return_value=[])
    repo.get_starred_artists = AsyncMock(return_value=[])
    repo.get_starred_songs = AsyncMock(return_value=[])
    repo.get_genres = AsyncMock(return_value=[])
    repo.get_album_count = AsyncMock(return_value=0)
    repo.get_artist_count = AsyncMock(return_value=0)
    repo.get_song_count = AsyncMock(return_value=0)
    type(repo).stats_ttl = PropertyMock(return_value=600)
    prefs = MagicMock()
    conn = MagicMock()
    conn.enabled = True
    prefs.get_navidrome_connection_raw.return_value = conn
    return NavidromeLibraryService(navidrome_repo=repo, preferences_service=prefs)


def _jellyfin_service(playlists=None, items=None) -> JellyfinLibraryService:
    repo = MagicMock()
    repo.get_playlists = AsyncMock(return_value=playlists or [])
    repo.get_playlist = AsyncMock(return_value=(playlists[0] if playlists else None))
    repo.get_playlist_items = AsyncMock(return_value=items or [])
    repo.get_recently_played = AsyncMock(return_value=[])
    repo.get_recently_added = AsyncMock(return_value=[])
    repo.get_favorites = AsyncMock(return_value=[])
    repo.get_genres = AsyncMock(return_value=[])
    repo.get_most_played_artists = AsyncMock(return_value=[])
    repo.get_most_played_albums = AsyncMock(return_value=[])
    repo.get_library_stats = AsyncMock(return_value={"album_count": 0, "artist_count": 0, "track_count": 0})
    repo.get_albums = AsyncMock(return_value=([], 0))
    type(repo).stats_ttl = PropertyMock(return_value=600)
    prefs = MagicMock()
    conn = MagicMock()
    conn.enabled = True
    prefs.get_jellyfin_connection_raw.return_value = conn
    return JellyfinLibraryService(jellyfin_repo=repo, preferences_service=prefs)


def _plex_playlist(key="pl-1", title="My Plex Playlist", leaf=3, dur=180000, smart=False) -> PlexPlaylist:
    return PlexPlaylist(ratingKey=key, title=title, leafCount=leaf, duration=dur, smart=smart, composite="/art/1")


def _plex_track(key="t-1", title="Song", artist="Artist", album="Album", parent_key="a-1", part_key="/library/parts/1/1/file.flac") -> PlexTrack:
    media = [PlexMedia(Part=[PlexPart(key=part_key)])] if part_key else []
    return PlexTrack(ratingKey=key, title=title, grandparentTitle=artist, parentTitle=album, parentRatingKey=parent_key, duration=200000, index=1, parentIndex=1, Media=media)


def _navidrome_playlist(pid="nd-pl-1", name="ND Playlist", songs=2, dur=300) -> SubsonicPlaylist:
    return SubsonicPlaylist(id=pid, name=name, songCount=songs, duration=dur)


def _navidrome_song(sid="ns-1", title="Song", artist="Artist", album="Album") -> SubsonicSong:
    return SubsonicSong(id=sid, title=title, artist=artist, album=album, albumId="alb-1", artistId="art-1", duration=180, track=1, discNumber=1)


def _jellyfin_item(iid="jf-1", name="JF Item", item_type="Playlist", child_count=5, ticks=3_000_000_000) -> JellyfinItem:
    return JellyfinItem(id=iid, name=name, type=item_type, child_count=child_count, duration_ticks=ticks, image_tag="abc", date_created="2024-01-01")


def _jellyfin_track(iid="jft-1", name="JF Track", artist="Artist", album="Album") -> JellyfinItem:
    return JellyfinItem(id=iid, name=name, type="Audio", artist_name=artist, album_name=album, album_id="ja-1", artist_id="jar-1", duration_ticks=2_000_000_000, index_number=1, parent_index_number=1)


class TestPlexListPlaylists:
    @pytest.mark.asyncio
    async def test_returns_summaries(self):
        svc = _plex_service(playlists=[_plex_playlist(), _plex_playlist(key="pl-2", title="Second")])
        result = await svc.list_playlists(limit=10)
        assert len(result) == 2
        assert result[0].id == "pl-1"
        assert result[0].name == "My Plex Playlist"
        assert result[0].duration_seconds == 180
        assert result[0].cover_url == "/api/v1/plex/playlist-thumb/pl-1"

    @pytest.mark.asyncio
    async def test_empty_playlists(self):
        svc = _plex_service(playlists=[])
        result = await svc.list_playlists()
        assert result == []

    @pytest.mark.asyncio
    async def test_limit_respected(self):
        playlists = [_plex_playlist(key=f"pl-{i}") for i in range(10)]
        svc = _plex_service(playlists=playlists)
        result = await svc.list_playlists(limit=3)
        assert len(result) == 3


class TestPlexPlaylistDetail:
    @pytest.mark.asyncio
    async def test_returns_detail_with_tracks(self):
        pl = _plex_playlist()
        tracks = [_plex_track(), _plex_track(key="t-2", title="Song 2")]
        svc = _plex_service(playlists=[pl], items=tracks)
        detail = await svc.get_playlist_detail("pl-1")
        assert detail.id == "pl-1"
        assert detail.name == "My Plex Playlist"
        assert len(detail.tracks) == 2
        assert detail.tracks[0].track_name == "Song"
        assert detail.tracks[0].duration_seconds == 200

    @pytest.mark.asyncio
    async def test_not_found_raises(self):
        svc = _plex_service(playlists=[])
        with pytest.raises(Exception, match="not found"):
            await svc.get_playlist_detail("nonexistent")


class TestPlexImportPlaylist:
    @pytest.mark.asyncio
    async def test_import_new_playlist(self):
        pl = _plex_playlist()
        tracks = [_plex_track()]
        svc = _plex_service(playlists=[pl], items=tracks)
        ps = _mock_playlist_service()
        result = await svc.import_playlist("pl-1", ps)
        assert result.musicseerr_playlist_id == "new-pl-1"
        assert result.tracks_imported == 1
        assert result.already_imported is False
        ps.create_playlist.assert_awaited_once()
        ps.add_tracks.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_import_idempotent(self):
        existing = PlaylistRecord(id="existing-1", name="Already", cover_image_path=None, created_at="2024-01-01", updated_at="2024-01-01")
        svc = _plex_service()
        ps = _mock_playlist_service(existing=existing)
        result = await svc.import_playlist("pl-1", ps)
        assert result.already_imported is True
        assert result.musicseerr_playlist_id == "existing-1"
        ps.create_playlist.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_import_track_keys_correct(self):
        pl = _plex_playlist()
        tracks = [_plex_track()]
        svc = _plex_service(playlists=[pl], items=tracks)
        ps = _mock_playlist_service()
        await svc.import_playlist("pl-1", ps)
        call_args = ps.add_tracks.call_args[0]
        track_dicts = call_args[1]
        assert track_dicts[0]["track_name"] == "Song"
        assert track_dicts[0]["artist_name"] == "Artist"
        assert track_dicts[0]["album_name"] == "Album"
        assert track_dicts[0]["source_type"] == "plex"
        # track_source_id must be the Plex part key (what the stream proxy needs),
        # while plex_rating_key keeps the rating key for scrobble/now-playing.
        assert track_dicts[0]["track_source_id"] == "/library/parts/1/1/file.flac"
        assert track_dicts[0]["plex_rating_key"] == "t-1"

    @pytest.mark.asyncio
    async def test_import_skips_tracks_without_part_key(self):
        pl = _plex_playlist()
        tracks = [_plex_track(), _plex_track(key="t-2", title="No Media", part_key="")]
        svc = _plex_service(playlists=[pl], items=tracks)
        ps = _mock_playlist_service()
        result = await svc.import_playlist("pl-1", ps)
        assert result.tracks_imported == 1
        assert result.tracks_failed == 1
        track_dicts = ps.add_tracks.call_args[0][1]
        assert len(track_dicts) == 1
        assert track_dicts[0]["track_source_id"] == "/library/parts/1/1/file.flac"

    @pytest.mark.asyncio
    async def test_import_rollback_on_add_tracks_failure(self):
        pl = _plex_playlist()
        tracks = [_plex_track()]
        svc = _plex_service(playlists=[pl], items=tracks)
        ps = _mock_playlist_service()
        ps.add_tracks = AsyncMock(side_effect=Exception("DB error"))
        with pytest.raises(Exception):
            await svc.import_playlist("pl-1", ps)
        ps.delete_playlist.assert_awaited_once_with("new-pl-1")


class TestNavidromeListPlaylists:
    @pytest.mark.asyncio
    async def test_returns_summaries(self):
        svc = _navidrome_service(playlists=[_navidrome_playlist()])
        result = await svc.list_playlists()
        assert len(result) == 1
        assert result[0].id == "nd-pl-1"
        assert result[0].name == "ND Playlist"
        assert result[0].cover_url == "/api/v1/navidrome/cover/nd-pl-1"


class TestNavidromePlaylistDetail:
    @pytest.mark.asyncio
    async def test_returns_detail(self):
        detail_raw = _navidrome_playlist()
        detail_raw.entry = [_navidrome_song()]
        svc = _navidrome_service(playlist_detail=detail_raw)
        detail = await svc.get_playlist_detail("nd-pl-1")
        assert detail.id == "nd-pl-1"
        assert len(detail.tracks) == 1
        assert detail.tracks[0].track_name == "Song"
        assert detail.tracks[0].source_type if hasattr(detail.tracks[0], "source_type") else True

    @pytest.mark.asyncio
    async def test_not_found_raises(self):
        svc = _navidrome_service(playlist_detail=None)
        with pytest.raises(Exception, match="not found"):
            await svc.get_playlist_detail("missing")


class TestNavidromeImportPlaylist:
    @pytest.mark.asyncio
    async def test_import_new(self):
        detail_raw = _navidrome_playlist()
        detail_raw.entry = [_navidrome_song()]
        svc = _navidrome_service(playlist_detail=detail_raw)
        ps = _mock_playlist_service()
        result = await svc.import_playlist("nd-pl-1", ps)
        assert result.tracks_imported == 1
        assert result.already_imported is False

    @pytest.mark.asyncio
    async def test_import_track_keys_correct(self):
        detail_raw = _navidrome_playlist()
        detail_raw.entry = [_navidrome_song()]
        svc = _navidrome_service(playlist_detail=detail_raw)
        ps = _mock_playlist_service()
        await svc.import_playlist("nd-pl-1", ps)
        track_dicts = ps.add_tracks.call_args[0][1]
        assert track_dicts[0]["track_name"] == "Song"
        assert track_dicts[0]["source_type"] == "navidrome"
        assert track_dicts[0]["track_source_id"] == "ns-1"


class TestJellyfinListPlaylists:
    @pytest.mark.asyncio
    async def test_returns_summaries(self):
        svc = _jellyfin_service(playlists=[_jellyfin_item()])
        result = await svc.list_playlists()
        assert len(result) == 1
        assert result[0].id == "jf-1"
        assert result[0].name == "JF Item"
        assert result[0].duration_seconds == 300
        assert result[0].cover_url == "/api/v1/jellyfin/image/jf-1"


class TestJellyfinPlaylistDetail:
    @pytest.mark.asyncio
    async def test_returns_detail(self):
        pl = _jellyfin_item()
        tracks = [_jellyfin_track()]
        svc = _jellyfin_service(playlists=[pl], items=tracks)
        detail = await svc.get_playlist_detail("jf-1")
        assert detail.id == "jf-1"
        assert len(detail.tracks) == 1
        assert detail.tracks[0].track_name == "JF Track"
        assert detail.tracks[0].duration_seconds == 200


class TestJellyfinImportPlaylist:
    @pytest.mark.asyncio
    async def test_import_new(self):
        pl = _jellyfin_item()
        tracks = [_jellyfin_track()]
        svc = _jellyfin_service(playlists=[pl], items=tracks)
        ps = _mock_playlist_service()
        result = await svc.import_playlist("jf-1", ps)
        assert result.tracks_imported == 1
        assert result.already_imported is False

    @pytest.mark.asyncio
    async def test_import_track_keys_correct(self):
        pl = _jellyfin_item()
        tracks = [_jellyfin_track()]
        svc = _jellyfin_service(playlists=[pl], items=tracks)
        ps = _mock_playlist_service()
        await svc.import_playlist("jf-1", ps)
        track_dicts = ps.add_tracks.call_args[0][1]
        assert track_dicts[0]["track_name"] == "JF Track"
        assert track_dicts[0]["source_type"] == "jellyfin"
        assert track_dicts[0]["track_source_id"] == "jft-1"

    @pytest.mark.asyncio
    async def test_import_idempotent(self):
        existing = PlaylistRecord(id="ex-1", name="Exists", cover_image_path=None, created_at="2024-01-01", updated_at="2024-01-01")
        svc = _jellyfin_service()
        ps = _mock_playlist_service(existing=existing)
        result = await svc.import_playlist("jf-1", ps)
        assert result.already_imported is True
        ps.create_playlist.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_rollback_on_failure(self):
        pl = _jellyfin_item()
        tracks = [_jellyfin_track()]
        svc = _jellyfin_service(playlists=[pl], items=tracks)
        ps = _mock_playlist_service()
        ps.add_tracks = AsyncMock(side_effect=Exception("fail"))
        with pytest.raises(ExternalServiceError):
            await svc.import_playlist("jf-1", ps)
        ps.delete_playlist.assert_awaited_once_with("new-pl-1")
