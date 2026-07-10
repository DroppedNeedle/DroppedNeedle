"""SmartPlaylistService: Smart Mix generation from artist / genre / mood seeds.

The radio-plan engine is exercised for real (with mocked repos/library_db, the
same fakes test_radio_plan_service uses); the playlist side uses a mocked repo
so persistence calls are observable.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import HTTPException

from core.exceptions import ValidationError
from repositories.playlist_repository import PlaylistRecord, PlaylistTrackRecord
from services.discover.radio_plan_service import RadioPlanService
from services.playlist_service import PlaylistService
from services.smart_playlist_service import (
    MAX_TRACK_COUNT,
    SmartPlaylistService,
)

_USER = SimpleNamespace(id="user-1", role="user")


def _lib_row(title: str, artist: str, artist_mbid: str, file_id: str):
    return {
        "id": file_id,
        "track_title": title,
        "artist_name": artist,
        "artist_mbid": artist_mbid,
        "album_artist_name": artist,
        "album_artist_mbid": artist_mbid,
        "recording_mbid": None,
        "release_group_mbid": "rg-1",
        "album_title": "Some Album",
        "file_format": "flac",
        "duration_seconds": 200.0,
    }


def _make_radio_plan(**overrides) -> RadioPlanService:
    lb = MagicMock()
    lb.get_similar_artists = AsyncMock(return_value=[])
    lb.get_artist_top_release_groups = AsyncMock(
        return_value=[SimpleNamespace(artist_name="Erykah Badu")]
    )
    lb.get_artist_top_recordings = AsyncMock(return_value=[])
    mb = MagicMock()
    mb.get_release_group = AsyncMock(return_value=None)
    mbid = MagicMock()
    mbid.normalize_mbid = staticmethod(lambda m: m.lower() if m else None)
    library_db = MagicMock()
    library_db.get_files_by_artist_mbids = AsyncMock(return_value=[])
    library_db.get_files_by_genre = AsyncMock(return_value=[])
    genre_index = MagicMock()
    genre_index.get_artists_for_genres = AsyncMock(return_value={})
    genre_index.get_genres_for_artists = AsyncMock(return_value={})
    lfm = MagicMock()
    lfm.get_tag_top_artists = AsyncMock(return_value=[])
    deps = dict(
        lb_repo=lb, mb_repo=mb, mbid_svc=mbid,
        library_db=library_db, genre_index=genre_index, lfm_repo=lfm,
    )
    deps.update(overrides)
    return RadioPlanService(**deps)


def _make_playlist_service(tmp_path):
    repo = MagicMock()
    created_tracks: list[dict] = []

    def _create_playlist(name, source_ref, user_id):
        return PlaylistRecord(
            id="pl-1", name=name, cover_image_path=None,
            created_at="2026-01-01T00:00:00+00:00", updated_at="2026-01-01T00:00:00+00:00",
            user_id=user_id,
        )

    def _add_tracks(playlist_id, tracks, position=None):
        created_tracks.extend(tracks)
        return [
            PlaylistTrackRecord(
                id=f"t-{i}", playlist_id=playlist_id, position=i,
                track_name=t["track_name"], artist_name=t["artist_name"],
                album_name=t["album_name"], album_id=t.get("album_id"),
                artist_id=t.get("artist_id"), track_source_id=t.get("track_source_id"),
                cover_url=t.get("cover_url"), source_type=t.get("source_type", ""),
                available_sources=t.get("available_sources"), format=t.get("format"),
                track_number=t.get("track_number"), disc_number=t.get("disc_number"),
                duration=t.get("duration"), created_at="2026-01-01T00:00:00+00:00",
            )
            for i, t in enumerate(tracks)
        ]

    repo.create_playlist = MagicMock(side_effect=_create_playlist)
    repo.add_tracks = MagicMock(side_effect=_add_tracks)
    get_playlist_calls = {}

    def _get_playlist(playlist_id):
        return get_playlist_calls.setdefault(
            playlist_id,
            PlaylistRecord(
                id=playlist_id, name="Mix", cover_image_path=None,
                created_at="2026-01-01T00:00:00+00:00",
                updated_at="2026-01-01T00:00:00+00:00",
                user_id=_USER.id,
            ),
        )

    repo.get_playlist = MagicMock(side_effect=_get_playlist)
    service = PlaylistService(repo=repo, cache_dir=tmp_path)
    return service, repo, created_tracks


def _make_service(tmp_path, radio_plan=None):
    radio_plan = radio_plan or _make_radio_plan()
    playlist_service, repo, created_tracks = _make_playlist_service(tmp_path)
    svc = SmartPlaylistService(
        radio_plan=radio_plan,
        playlist_service=playlist_service,
        genre_index=radio_plan._genre_index,
        library_db=radio_plan._library_db,
    )
    return svc, radio_plan, repo, created_tracks


class TestArtistSeed:
    @pytest.mark.asyncio
    async def test_persists_playlist_with_library_tracks(self, tmp_path):
        radio = _make_radio_plan()
        radio._library_db.get_files_by_artist_mbids = AsyncMock(
            return_value=[_lib_row(f"T{i}", f"Artist{i}", f"a-{i}", f"f-{i}") for i in range(12)]
        )
        svc, _, repo, created = _make_service(tmp_path, radio)
        playlist, tracks = await svc.generate(
            _USER, seed_type="artist", seed="a-0", count=10,
        )
        assert playlist.id == "pl-1"
        assert playlist.name == "Erykah Badu — Smart Mix"
        repo.create_playlist.assert_called_once()
        assert 0 < len(tracks) <= 10
        assert all(t["source_type"] == "local" for t in created)
        assert all(t["track_source_id"] for t in created)
        assert all(t["library_file_id"] for t in created)

    @pytest.mark.asyncio
    async def test_explicit_name_wins(self, tmp_path):
        radio = _make_radio_plan()
        radio._library_db.get_files_by_artist_mbids = AsyncMock(
            return_value=[_lib_row("T", "A", "a-0", "f-0")]
        )
        svc, _, _, _ = _make_service(tmp_path, radio)
        playlist, _ = await svc.generate(
            _USER, seed_type="artist", seed="a-0", count=10, name="My Custom Mix",
        )
        assert playlist.name == "My Custom Mix"

    @pytest.mark.asyncio
    async def test_empty_library_returns_422(self, tmp_path):
        svc, _, _, _ = _make_service(tmp_path)  # library_db returns no rows
        with pytest.raises(HTTPException) as exc:
            await svc.generate(_USER, seed_type="artist", seed="a-0", count=10)
        assert exc.value.status_code == 422

    @pytest.mark.asyncio
    async def test_empty_seed_rejected(self, tmp_path):
        svc, _, _, _ = _make_service(tmp_path)
        with pytest.raises(ValidationError):
            await svc.generate(_USER, seed_type="artist", seed="   ", count=10)


class TestGenreSeed:
    @pytest.mark.asyncio
    async def test_persists_playlist_from_genre_index_and_file_tags(self, tmp_path):
        radio = _make_radio_plan()
        radio._genre_index.get_artists_for_genres = AsyncMock(
            return_value={"neo soul": ["a-1", "a-2"]}
        )
        radio._library_db.get_files_by_artist_mbids = AsyncMock(
            return_value=[_lib_row(f"T{i}", f"Artist{i}", f"a-{i}", f"f-{i}") for i in range(8)]
        )
        radio._library_db.get_files_by_genre = AsyncMock(
            return_value=[_lib_row(f"G{i}", f"TagArtist{i}", f"ta-{i}", f"g-{i}") for i in range(8)]
        )
        svc, _, _, created = _make_service(tmp_path, radio)
        playlist, tracks = await svc.generate(
            _USER, seed_type="genre", seed="neo soul", count=15,
        )
        assert playlist.name == "Neo Soul — Smart Mix"
        assert 0 < len(tracks) <= 15
        assert all(t["source_type"] == "local" for t in created)

    @pytest.mark.asyncio
    async def test_count_cap_respected(self, tmp_path):
        radio = _make_radio_plan()
        radio._genre_index.get_artists_for_genres = AsyncMock(
            return_value={"rock": [f"a-{i}" for i in range(40)]}
        )
        radio._library_db.get_files_by_artist_mbids = AsyncMock(
            return_value=[
                _lib_row(f"T{i}", f"Artist{i % 60}", f"a-{i % 60}", f"f-{i}") for i in range(400)
            ]
        )
        svc, _, _, _ = _make_service(tmp_path, radio)
        _, tracks = await svc.generate(
            _USER, seed_type="genre", seed="rock", count=5000,
        )
        assert len(tracks) <= MAX_TRACK_COUNT

    @pytest.mark.asyncio
    async def test_max_count_reachable_on_deep_library(self, tmp_path):
        # 250 is the new cap; a deep enough pool should actually fill it
        radio = _make_radio_plan()
        radio._genre_index.get_artists_for_genres = AsyncMock(
            return_value={"rock": [f"a-{i}" for i in range(120)]}
        )
        radio._library_db.get_files_by_artist_mbids = AsyncMock(
            return_value=[
                _lib_row(f"T{i}", f"Artist{i % 150}", f"a-{i % 150}", f"f-{i}")
                for i in range(600)
            ]
        )
        svc, _, _, _ = _make_service(tmp_path, radio)
        _, tracks = await svc.generate(
            _USER, seed_type="genre", seed="rock", count=250,
        )
        assert len(tracks) == MAX_TRACK_COUNT == 250

    @pytest.mark.asyncio
    async def test_short_pool_returns_available_instead_of_erroring(self, tmp_path):
        # a 250 request on a small library returns what the library has
        radio = _make_radio_plan()
        radio._genre_index.get_artists_for_genres = AsyncMock(
            return_value={"rock": ["a-0", "a-1"]}
        )
        radio._library_db.get_files_by_artist_mbids = AsyncMock(
            return_value=[_lib_row(f"T{i}", f"Artist{i}", f"a-{i}", f"f-{i}") for i in range(7)]
        )
        svc, _, _, _ = _make_service(tmp_path, radio)
        playlist, tracks = await svc.generate(
            _USER, seed_type="genre", seed="rock", count=250,
        )
        assert playlist.id == "pl-1"
        assert len(tracks) == 7


class TestMoodSeed:
    @pytest.mark.asyncio
    async def test_mood_maps_to_matched_library_tags(self, tmp_path):
        radio = _make_radio_plan()

        async def _artists_for(genres):
            # only "jazz" and "ambient" exist in this library
            return {g.strip().lower(): (["a-1"] if g in ("jazz", "ambient") else [])
                    for g in genres}

        radio._genre_index.get_artists_for_genres = AsyncMock(side_effect=_artists_for)
        radio._library_db.get_files_by_artist_mbids = AsyncMock(
            return_value=[_lib_row(f"T{i}", f"Artist{i}", f"a-{i}", f"f-{i}") for i in range(10)]
        )
        radio._library_db.get_files_by_genre = AsyncMock(return_value=[])
        svc, _, _, _ = _make_service(tmp_path, radio)
        playlist, tracks = await svc.generate(
            _USER, seed_type="mood", seed="chill", count=10,
        )
        assert playlist.name == "Chill — Smart Mix"
        assert len(tracks) > 0

    @pytest.mark.asyncio
    async def test_unknown_mood_returns_422(self, tmp_path):
        svc, _, _, _ = _make_service(tmp_path)
        with pytest.raises(HTTPException) as exc:
            await svc.generate(_USER, seed_type="mood", seed="grumpy", count=10)
        assert exc.value.status_code == 422
        assert "Unknown mood" in exc.value.detail

    @pytest.mark.asyncio
    async def test_mood_with_no_library_matches_returns_422(self, tmp_path):
        svc, _, _, _ = _make_service(tmp_path)  # index + file tags both empty
        with pytest.raises(HTTPException) as exc:
            await svc.generate(_USER, seed_type="mood", seed="workout", count=10)
        assert exc.value.status_code == 422
        assert "workout" in exc.value.detail

    @pytest.mark.asyncio
    async def test_mood_matches_via_file_tags_when_index_empty(self, tmp_path):
        radio = _make_radio_plan()
        radio._genre_index.get_artists_for_genres = AsyncMock(return_value={})

        async def _files_by_genre(genre, limit=200):
            if genre == "jazz":
                return [_lib_row(f"J{i}", f"JazzCat{i}", f"j-{i}", f"jf-{i}") for i in range(6)]
            return []

        radio._library_db.get_files_by_genre = AsyncMock(side_effect=_files_by_genre)
        svc, _, _, created = _make_service(tmp_path, radio)
        _, tracks = await svc.generate(_USER, seed_type="mood", seed="chill", count=10)
        assert len(tracks) > 0
        assert all(t["source_type"] == "local" for t in created)
