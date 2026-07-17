"""Tests for precache phase classes - construction and basic behavior."""

import pytest
from unittest.mock import AsyncMock, MagicMock

from services.precache.artist_phase import ArtistPhase
from services.precache.album_phase import AlbumPhase
from services.precache.audiodb_phase import AudioDBPhase
from services.precache.orchestrator import LibraryPrecacheService
from infrastructure.queue.priority_queue import RequestPriority


class TestPhaseConstruction:
    def test_artist_phase_constructs(self):
        phase = ArtistPhase(
            library_repo=AsyncMock(),
            cover_repo=AsyncMock(),
            preferences_service=MagicMock(),
            genre_index=AsyncMock(),
            sync_state_store=AsyncMock(),
        )
        assert phase is not None

    @pytest.mark.asyncio
    async def test_album_phase_uses_background_priorities(self, monkeypatch, tmp_path):
        album_service = MagicMock()
        album_service._cache.get = AsyncMock(return_value=None)
        album_service.get_album_info = AsyncMock()
        monkeypatch.setattr(
            "core.dependencies.get_album_service", lambda: album_service
        )
        cover_repo = MagicMock()
        cover_repo.cache_dir = tmp_path
        cover_repo.get_release_group_cover = AsyncMock()
        preferences = MagicMock()
        preferences.get_advanced_settings.return_value = MagicMock(
            batch_albums=1,
            delay_albums=0,
        )
        sync_state = MagicMock()
        sync_state.mark_items_processed_batch = AsyncMock()
        status = MagicMock()
        status.is_cancelled.return_value = False
        status.update_progress = AsyncMock()
        status.persist_progress = AsyncMock()
        release_group_id = "11111111-1111-1111-1111-111111111111"
        phase = AlbumPhase(cover_repo, preferences, sync_state)

        await phase.precache_album_data(
            [release_group_id],
            {release_group_id},
            status,
        )

        album_service.get_album_info.assert_awaited_once_with(
            release_group_id,
            library_mbids={release_group_id},
            priority=RequestPriority.BACKGROUND_SYNC,
        )
        cover_repo.get_release_group_cover.assert_awaited_once_with(
            release_group_id,
            size="500",
            priority=RequestPriority.BACKGROUND_SYNC,
        )

    def test_album_phase_constructs(self):
        phase = AlbumPhase(
            cover_repo=AsyncMock(),
            preferences_service=MagicMock(),
            sync_state_store=AsyncMock(),
        )
        assert phase is not None

    def test_audiodb_phase_constructs(self):
        phase = AudioDBPhase(
            cover_repo=AsyncMock(),
            preferences_service=MagicMock(),
            audiodb_image_service=AsyncMock(),
        )
        assert phase is not None

    def test_orchestrator_constructs_and_creates_phases(self):
        svc = LibraryPrecacheService(
            library_repo=AsyncMock(),
            cover_repo=AsyncMock(),
            preferences_service=MagicMock(),
            sync_state_store=AsyncMock(),
            genre_index=AsyncMock(),
            library_db=AsyncMock(),
            audiodb_image_service=AsyncMock(),
        )
        assert isinstance(svc._artist_phase, ArtistPhase)
        assert isinstance(svc._album_phase, AlbumPhase)
        assert isinstance(svc._audiodb_phase, AudioDBPhase)


class TestOrchestratorDelegation:
    def test_sort_by_cover_priority_delegates(self, tmp_path):
        cover_repo = MagicMock()
        cover_repo.cache_dir = tmp_path
        svc = LibraryPrecacheService(
            library_repo=AsyncMock(),
            cover_repo=cover_repo,
            preferences_service=MagicMock(),
            sync_state_store=AsyncMock(),
            genre_index=AsyncMock(),
            library_db=AsyncMock(),
        )
        items = [{"mbid": "a", "name": "A"}, {"mbid": "b", "name": "B"}]
        result = svc._sort_by_cover_priority(items, "artist")
        assert len(result) == 2

    @pytest.mark.asyncio
    async def test_check_cache_needs_delegates(self):
        audiodb_svc = AsyncMock()
        audiodb_svc.get_cached_artist_images = AsyncMock(return_value=None)
        audiodb_svc.get_cached_album_images = AsyncMock(return_value=None)

        svc = LibraryPrecacheService(
            library_repo=AsyncMock(),
            cover_repo=AsyncMock(),
            preferences_service=MagicMock(),
            sync_state_store=AsyncMock(),
            genre_index=AsyncMock(),
            library_db=AsyncMock(),
            audiodb_image_service=audiodb_svc,
        )

        artists = [{"mbid": "cc197bad-dc9c-440d-a5b5-d52ba2e14234", "name": "Test"}]
        needed_artists, needed_albums = await svc._check_audiodb_cache_needs(artists, [])
        assert len(needed_artists) == 1
        assert len(needed_albums) == 0


class TestShimIdentity:
    def test_shim_reexports_same_class(self):
        from services.library_precache_service import LibraryPrecacheService as ShimClass
        from services.precache.orchestrator import LibraryPrecacheService as RealClass
        assert ShimClass is RealClass

    def test_home_shim_reexports_same_class(self):
        from services.home_service import HomeService as ShimClass
        from services.home.facade import HomeService as RealClass
        assert ShimClass is RealClass

    def test_charts_shim_reexports_same_class(self):
        from services.home_charts_service import HomeChartsService as ShimClass
        from services.home.charts_service import HomeChartsService as RealClass
        assert ShimClass is RealClass
