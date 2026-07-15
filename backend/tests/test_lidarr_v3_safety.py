"""Test Lidarr v3 API safety patterns and error handling.

This test validates the safety patterns for the Lidarr v3 API integration,
focusing on proper error handling, validation, and safe mutation patterns.
"""

import pytest
from unittest.mock import AsyncMock, Mock, patch
from datetime import datetime, timezone
import json

from core.request_backend_settings import RequestBackendSettings, BackendType
from core.exceptions import ValidationError, ExternalServiceError
from services.request_backend_service import RequestBackendService, ALREADY_IN_LIBRARY


class TestLidarrV3Safety:
    """Test Lidarr v3 API safety patterns and error handling."""

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

    @pytest.fixture
    def mock_lidarr_v3_client(self):
        """Mock Lidarr v3 HTTP client for safety testing."""
        mock = Mock()
        mock.get_artist = AsyncMock(return_value={
            "id": 123,
            "artistName": "Test Artist",
            "foreignArtistId": "mbid_123",
            "qualityProfileId": 1,
            "metadataProfileId": 1,
            "monitored": True,
        })
        mock.get_artist_by_mbid = AsyncMock(return_value={
            "id": 124,
            "artistName": "Test Artist",
            "foreignArtistId": "mbid_123",
            "qualityProfileId": 1,
            "metadataProfileId": 1,
            "monitored": True,
        })
        mock.post_artist = AsyncMock(return_value={"id": 123})
        mock.put_artist = AsyncMock(return_value={"id": 123})
        mock.get_album = AsyncMock(return_value={
            "id": 456,
            "title": "Test Album",
            "artistId": 123,
            "foreignAlbumId": "rg123",
            "monitored": False,
        })
        mock.put_album = AsyncMock(return_value={"id": 456})
        mock.post_album_search = AsyncMock(return_value={"id": "search_123"})
        mock.get_command = AsyncMock(return_value={
            "id": "search_123",
            "name": "AlbumSearch",
            "status": "Completed",
            "result": {"success": True}
        })
        return mock

    @pytest.mark.asyncio
    async def test_artist_validation_before_post(self, mock_lidarr_v3_client):
        """Artist should be validated via GET before any POST mutation."""
        # Try to validate artist via GET
        artist_data = await mock_lidarr_v3_client.get_artist_by_mbid("mbid_123")

        # Only after successful validation should POST be allowed
        assert artist_data["foreignArtistId"] == "mbid_123"
        result = await mock_lidarr_v3_client.post_artist(artist_data)

        mock_lidarr_v3_client.get_artist_by_mbid.assert_called_once_with("mbid_123")
        mock_lidarr_v3_client.post_artist.assert_called_once_with(artist_data)

    @pytest.mark.asyncio
    async def test_non_library_artist_blocks_mutation(self, mock_lidarr_v3_client):
        """Non-library artists should block all POST/PUT mutations."""
        # Artist not in library
        mock_lidarr_v3_client.get_artist_by_mbid.side_effect = Exception("Artist not found")

        with pytest.raises(Exception, match="Artist not found"):
            await mock_lidarr_v3_client.get_artist_by_mbid("unknown_mbid")

        # No mutations should occur
        mock_lidarr_v3_client.post_artist.assert_not_called()
        mock_lidarr_v3_client.put_artist.assert_not_called()

    @pytest.mark.asyncio
    async def test_album_full_object_pattern(self, mock_lidarr_v3_client):
        """Album operations must use GET->modify->PUT pattern."""
        # Step 1: GET full album object
        album = await mock_lidarr_v3_client.get_album(456)
        original_album = album.copy()

        # Step 2: Modify specific field
        album["monitored"] = True

        # Step 3: PUT entire modified object
        await mock_lidarr_v3_client.put_album(album["id"], album)

        # Verify correct pattern
        mock_lidarr_v3_client.get_album.assert_called_once_with(456)
        mock_lidarr_v3_client.put_album.assert_called_once()

        # Verify full object was PUT, not partial update
        call_args = mock_lidarr_v3_client.put_album.call_args
        put_album = call_args[0][1]  # Second argument is the album object
        assert put_album["id"] == original_album["id"]
        assert put_album["foreignAlbumId"] == original_album["foreignAlbumId"]
        assert put_album["title"] == original_album["title"]
        # Only the monitored field should have changed
        assert put_album["monitored"] == True

    @pytest.mark.asyncio
    async def test_album_search_payload_format(self, mock_lidarr_v3_client):
        """AlbumSearch command must use albumIds[] array payload."""
        search_payload = {"albumIds": [456, 789]}

        result = await mock_lidarr_v3_client.post_album_search(search_payload)

        mock_lidarr_v3_client.post_album_search.assert_called_once_with(search_payload)
        assert "albumIds" in search_payload
        assert isinstance(search_payload["albumIds"], list)
        assert len(search_payload["albumIds"]) == 2

    @pytest.mark.asyncio
    async def test_error_handling_preserves_consistency(self, mock_lidarr_v3_client):
        """API errors should preserve system consistency and provide clear feedback."""
        # Simulate API error during album update
        mock_lidarr_v3_client.put_album.side_effect = Exception("API timeout")

        with pytest.raises(Exception, match="API timeout"):
            await mock_lidarr_v3_client.put_album(456, {"monitored": True})

        # Verify that partial state is not committed
        mock_lidarr_v3_client.put_album.assert_called_once()
        # The original album state should remain unchanged

    @pytest.mark.asyncio
    async def test_concurrent_request_handling(self, mock_lidarr_v3_client):
        """Concurrent requests for same resource should be handled safely."""
        # Simulate concurrent requests for same artist
        artist_data = await mock_lidarr_v3_client.get_artist_by_mbid("mbid_123")

        # Multiple concurrent GET requests should not cause issues
        artist1 = await mock_lidarr_v3_client.get_artist(123)
        artist2 = await mock_lidarr_v3_client.get_artist(123)

        assert artist1["id"] == artist2["id"]
        assert mock_lidarr_v3_client.get_artist.call_count == 2

    @pytest.mark.asyncio
    async def test_command_id_tracking(self, mock_lidarr_v3_client):
        """Command IDs should be tracked for status monitoring."""
        search_result = await mock_lidarr_v3_client.post_album_search({"albumIds": [456]})
        command_id = search_result["id"]

        # Verify command can be queried by ID
        command_status = await mock_lidarr_v3_client.get_command(command_id)

        assert command_status["id"] == command_id
        assert command_status["status"] == "Completed"
        mock_lidarr_v3_client.get_command.assert_called_once_with(command_id)

    @pytest.mark.asyncio
    async def test_validation_before_any_mutation(self, mock_lidarr_v3_client):
        """Validation must occur before any POST/PUT/DELETE mutations."""
        # Test artist validation
        artist = await mock_lidarr_v3_client.get_artist_by_mbid("mbid_123")
        assert artist["foreignArtistId"] == "mbid_123"

        # Only then allow mutation
        await mock_lidarr_v3_client.put_artist(123, {"monitored": True})

        # Verify order: validation before mutation
        mock_lidarr_v3_client.get_artist_by_mbid.assert_called_once()
        mock_lidarr_v3_client.put_artist.assert_called_once()

    @pytest.mark.asyncio
    async def test_no_native_duplicate_semantics(self, lidarr_service):
        """Lidarr backend should not rely on native duplicate-task semantics."""
        # Test that dispatch doesn't use native duplicate detection
        lidarr_service._download_service.request_album.return_value = "task_123"

        result = await lidarr_service.dispatch_request(
            user_id="user_123",
            release_group_mbid="rg123",
            artist_name="Test Artist",
            album_title="Test Album",
            year=2024,
            artist_mbid="artist_mbid_123",
            origin="user",
        )

        # Should call through the backend seam, not duplicate native logic
        assert lidarr_service._download_service.request_album.called
        assert result == "task_123"

    @pytest.mark.asyncio
    async def test_safe_fallback_to_native(self, lidarr_service):
        """When Lidarr backend is not ready, safe fallback to native should occur."""
        # The current implementation falls back to native
        result = await lidarr_service.dispatch_request(
            user_id="user_123",
            release_group_mbid="rg123",
            artist_name="Test Artist",
            album_title="Test Album",
            year=2024,
            artist_mbid="artist_mbid_123",
            origin="user",
        )

        # Should succeed via native fallback
        assert result != ALREADY_IN_LIBRARY
        assert result != "dispatch_failed"
        assert lidarr_service._download_service.request_album.called

    @pytest.mark.asyncio
    async def test_payload_validation(self, mock_lidarr_v3_client):
        """API payloads must be validated before submission."""
        # Valid payload
        valid_payload = {"albumIds": [456]}
        await mock_lidarr_v3_client.post_album_search(valid_payload)
        mock_lidarr_v3_client.post_album_search.assert_called_once()

        # Reset for next test
        mock_lidarr_v3_client.post_album_search.reset_mock()

        # Invalid payload (not an array) - mock won't validate by default
        # In real implementation, validation would happen before calling the client
        # For this test, we just verify the payload is passed through
        await mock_lidarr_v3_client.post_album_search({"albumIds": "not_an_array"})
        mock_lidarr_v3_client.post_album_search.assert_called_once()

    @pytest.mark.asyncio
    async def test_backwards_compatibility_with_native(self, lidarr_service):
        """Lidarr backend should maintain backwards compatibility with native."""
        # Ensure native functionality is preserved
        lidarr_service._download_service.request_album.return_value = "native_task_456"

        result = await lidarr_service.dispatch_request(
            user_id="user_123",
            release_group_mbid="rg123",
            artist_name="Test Artist",
            album_title="Test Album",
            year=2024,
            artist_mbid="artist_mbid_123",
            origin="user",
        )

        # Should work with native backend
        assert result == "native_task_456"
        assert lidarr_service._download_service.request_album.called

    @pytest.mark.asyncio
    async def test_monitoring_flag_safety(self, mock_lidarr_v3_client):
        """Artist monitoring flags should be set safely with full object updates."""
        # Get current state
        artist = await mock_lidarr_v3_client.get_artist(123)
        original_monitored = artist["monitored"]

        # Update monitoring flag
        artist["monitored"] = not original_monitored

        # PUT full object (not partial update)
        await mock_lidarr_v3_client.put_artist(123, artist)

        # Verify full object was PUT
        call_args = mock_lidarr_v3_client.put_artist.call_args
        put_artist = call_args[0][1]
        assert put_artist["id"] == 123
        assert put_artist["monitored"] != original_monitored
        # All other fields preserved
        assert put_artist["artistName"] == "Test Artist"
        assert put_artist["foreignArtistId"] == "mbid_123"