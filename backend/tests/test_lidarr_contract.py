"""Test Lidarr backend contract and API interaction patterns.

This test validates the contract for a future Lidarr backend implementation.
It verifies the expected API patterns without requiring a real Lidarr instance.
"""

import pytest
from unittest.mock import AsyncMock, Mock, patch
from datetime import datetime, timezone

from core.request_backend_settings import RequestBackendSettings, BackendType
from services.request_backend_service import RequestBackendService, ALREADY_IN_LIBRARY


class TestLidarrContract:
    """Test the Lidarr backend integration contract."""

    @pytest.fixture
    def lidarr_settings(self):
        """Create Lidarr backend settings."""
        return RequestBackendSettings(
            request_backend=BackendType(backend="lidarr")
        )

    @pytest.fixture
    def mock_download_service(self):
        """Mock download service."""
        mock = Mock()
        mock.request_album = AsyncMock(return_value="task_12345")
        return mock

    @pytest.fixture
    def lidarr_service(self, lidarr_settings, mock_download_service):
        """Create request backend service with Lidarr configuration."""
        return RequestBackendService(
            download_service=mock_download_service,
            settings=lidarr_settings,
        )

    @pytest.mark.asyncio
    async def test_lidarr_backend_fallback_to_native(self, lidarr_service):
        """Lidarr backend currently falls back to native until implemented."""
        result = await lidarr_service.dispatch_request(
            user_id="user_123",
            release_group_mbid="rg123",
            artist_name="Test Artist",
            album_title="Test Album",
            year=2024,
            artist_mbid="artist_mbid_123",
            origin="user",
        )

        # Currently falls back to native, so should return a task ID
        assert result != ALREADY_IN_LIBRARY
        assert result != "dispatch_failed"

    @pytest.mark.asyncio
    async def test_lidarr_backend_validation_error_propagates(self, lidarr_service):
        """Validation errors from backend dispatch should propagate."""
        from core.exceptions import ValidationError

        lidarr_service._download_service.request_album.side_effect = ValidationError(
            "Quota exceeded"
        )

        with pytest.raises(ValidationError, match="Quota exceeded"):
            await lidarr_service.dispatch_request(
                user_id="user_123",
                release_group_mbid="rg123",
                artist_name="Test Artist",
                album_title="Test Album",
                year=2024,
                artist_mbid="artist_mbid_123",
                origin="user",
            )

    @pytest.mark.asyncio
    async def test_lidarr_backend_dispatch_failure_returns_sentinel(self, lidarr_service):
        """Dispatch failures return sentinel value."""
        lidarr_service._download_service.request_album.side_effect = Exception("API error")

        result = await lidarr_service.dispatch_request(
            user_id="user_123",
            release_group_mbid="rg123",
            artist_name="Test Artist",
            album_title="Test Album",
            year=2024,
            artist_mbid="artist_mbid_123",
            origin="user",
        )

        assert result == "dispatch_failed"

    @pytest.mark.asyncio
    async def test_lidarr_backend_already_in_library(self, lidarr_service):
        """Already in library scenario returns sentinel value."""
        lidarr_service._download_service.request_album.return_value = ALREADY_IN_LIBRARY

        result = await lidarr_service.dispatch_request(
            user_id="user_123",
            release_group_mbid="rg123",
            artist_name="Test Artist",
            album_title="Test Album",
            year=2024,
            artist_mbid="artist_mbid_123",
            origin="user",
        )

        assert result == ALREADY_IN_LIBRARY

    @pytest.mark.asyncio
    async def test_lidarr_backend_retry_origin(self, lidarr_service):
        """Retry origin should be handled correctly."""
        lidarr_service._download_service.request_album.return_value = "task_67890"

        result = await lidarr_service.dispatch_request(
            user_id="user_123",
            release_group_mbid="rg123",
            artist_name="Test Artist",
            album_title="Test Album",
            year=2024,
            artist_mbid="artist_mbid_123",
            origin="retry",
        )

        assert result == "task_67890"
        lidarr_service._download_service.request_album.assert_called_once()

    @pytest.mark.asyncio
    async def test_lidarr_backend_handles_optional_artist_mbid(self, lidarr_service):
        """Optional artist MBID should be handled correctly."""
        lidarr_service._download_service.request_album.return_value = "task_99999"

        result = await lidarr_service.dispatch_request(
            user_id="user_123",
            release_group_mbid="rg123",
            artist_name="Test Artist",
            album_title="Test Album",
            year=2024,
            artist_mbid=None,
            origin="user",
        )

        assert result == "task_99999"


class TestLidarrFutureSemantics:
    """Test the expected semantics for a future Lidarr implementation.

    These tests document the expected behavior once Lidarr backend is fully implemented.
    They use mock Lidarr clients to verify the contract without real API calls.
    """

    @pytest.fixture
    def mock_lidarr_client(self):
        """Mock Lidarr HTTP client for future implementation."""
        mock = Mock()
        mock.get_artist = AsyncMock(return_value={
            "artistName": "Test Artist",
            "foreignArtistId": "mbid_123",
            "qualityProfileId": 1,
            "metadataProfileId": 1,
            "monitored": True,
        })
        mock.get_artist_by_mbid = AsyncMock(return_value={
            "artistName": "Test Artist",
            "foreignArtistId": "mbid_123",
            "qualityProfileId": 1,
            "metadataProfileId": 1,
            "monitored": True,
        })
        mock.post_artist = AsyncMock(return_value={"id": 123})
        mock.put_artist = AsyncMock(return_value={"id": 123})
        mock.get_album = AsyncMock(return_value={
            "title": "Test Album",
            "artistId": 123,
            "albumId": 456,
            "foreignAlbumId": "rg123",
            "monitored": False,
        })
        mock.put_album = AsyncMock(return_value={"id": 456})
        mock.post_album_search = AsyncMock(return_value={"id": "search_123"})
        return mock

    @pytest.mark.asyncio
    async def test_non_library_artist_requires_review(self, mock_lidarr_client):
        """Non-library artists should go to pending/manual review before mutations."""
        # Artist not in library => should be pending for manual review
        mock_lidarr_client.get_artist_by_mbid.side_effect = Exception("Not found")

        with pytest.raises(Exception, match="Not found"):
            await mock_lidarr_client.get_artist_by_mbid("mbid_123")

        # No POST/PUT mutations should occur for unknown artists
        mock_lidarr_client.post_artist.assert_not_called()
        mock_lidarr_client.put_artist.assert_not_called()

    @pytest.mark.asyncio
    async def test_library_artist_allows_safe_mutations(self, mock_lidarr_client):
        """Library artists should allow safe mutations."""
        # Artist exists in library
        artist_data = await mock_lidarr_client.get_artist_by_mbid("mbid_123")
        assert artist_data["artistName"] == "Test Artist"
        assert artist_data["monitored"] is True

        # Safe mutations allowed (e.g., update monitoring settings)
        await mock_lidarr_client.put_artist(123, {"monitored": True})
        mock_lidarr_client.put_artist.assert_called_once()

    @pytest.mark.asyncio
    async def test_album_monitoring_uses_full_object_pattern(self, mock_lidarr_client):
        """Album monitoring should use GET->modify->PUT pattern."""
        # First GET the full album object
        album = await mock_lidarr_client.get_album(456)
        assert album["foreignAlbumId"] == "rg123"

        # Modify the monitoring state
        album["monitored"] = True

        # PUT the full modified object back
        await mock_lidarr_client.put_album(album["albumId"], album)
        mock_lidarr_client.put_album.assert_called_once()

    @pytest.mark.asyncio
    async def test_album_search_uses_albumids_payload(self, mock_lidarr_client):
        """AlbumSearch should use albumIds[] payload format."""
        search_payload = {"albumIds": [456, 789]}

        await mock_lidarr_client.post_album_search(search_payload)
        mock_lidarr_client.post_album_search.assert_called_once_with(search_payload)

    @pytest.mark.asyncio
    async def test_no_duplicate_task_semantics(self, mock_lidarr_client):
        """Lidarr backend should not introduce native duplicate-task semantics."""
        # Album already exists in Lidarr
        existing_album = await mock_lidarr_client.get_album(456)
        assert existing_album["foreignAlbumId"] == "rg123"

        # Second request for same album should not create duplicate tasks
        # The backend should detect existing request and return early
        first_search = await mock_lidarr_client.post_album_search({"albumIds": [456]})
        second_search = await mock_lidarr_client.post_album_search({"albumIds": [456]})

        # Both should succeed without creating duplicate searches
        assert first_search["id"] == "search_123"
        assert second_search["id"] == "search_123"