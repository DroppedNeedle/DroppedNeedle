"""Test the request backend dependency injection wiring."""

import pytest
from unittest.mock import Mock, patch, MagicMock
from pathlib import Path

from core.dependencies.backend_providers import (
    get_request_backend_settings,
    get_request_backend_service,
)
from core.dependencies._registry import clear_all_singletons
from core.request_backend_settings import RequestBackendSettings, BackendType


class TestRequestBackendDI:
    """Test that the request backend dependency providers work correctly."""

    def setup_method(self):
        """Clear singleton cache before each test."""
        clear_all_singletons()

    @pytest.mark.asyncio
    async def test_get_request_backend_settings_default(self):
        """Default settings return native backend when no config present."""
        settings = get_request_backend_settings()
        assert isinstance(settings, RequestBackendSettings)
        assert settings.request_backend.backend == "native"

    def test_request_backend_settings_direct_construction(self):
        """Direct construction of RequestBackendSettings works correctly."""
        # Test default (native)
        settings = RequestBackendSettings()
        assert settings.request_backend.backend == "native"

        # Test explicit native
        settings = RequestBackendSettings(
            request_backend=BackendType(backend="native")
        )
        assert settings.request_backend.backend == "native"

        # Test lidarr
        settings = RequestBackendSettings(
            request_backend=BackendType(backend="lidarr")
        )
        assert settings.request_backend.backend == "lidarr"

    @pytest.mark.asyncio
    async def test_get_request_backend_service_native(self):
        """Service instance is created with native backend by default."""
        # Mock DownloadService to avoid full dependency chain
        mock_download_service = Mock()

        with patch(
            "core.dependencies.service_providers.get_download_service",
            return_value=mock_download_service
        ):
            service = get_request_backend_service()
            assert service is not None
            # The service should have been initialized with the download service
            assert service._download_service == mock_download_service
            assert service._settings.request_backend.backend == "native"

    @pytest.mark.asyncio
    async def test_get_request_backend_service_with_lidarr_settings(self):
        """Service instance is created with Lidarr backend when configured directly."""
        # Clear singleton cache first
        clear_all_singletons()

        mock_download_service = Mock()

        # Create Lidarr settings directly instead of going through config loading
        lidarr_settings = RequestBackendSettings(
            request_backend=BackendType(backend="lidarr")
        )

        # Mock get_request_backend_settings to return our Lidarr settings
        with patch(
            "core.dependencies.backend_providers.get_request_backend_settings",
            return_value=lidarr_settings
        ), patch(
            "core.dependencies.service_providers.get_download_service",
            return_value=mock_download_service
        ):
            # Clear the singleton so it picks up our mocked provider
            get_request_backend_service.cache_clear()
            service = get_request_backend_service()
            assert service is not None
            assert service._download_service == mock_download_service
            assert service._settings.request_backend.backend == "lidarr"