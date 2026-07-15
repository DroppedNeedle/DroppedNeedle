"""Request backend dependency provider."""

from core.dependencies._registry import singleton
from core.request_backend_settings import RequestBackendSettings


@singleton
def get_request_backend_settings() -> RequestBackendSettings:
    """Load request backend settings from config.json."""
    from core.config import get_settings
    from infrastructure.file_utils import read_json

    settings = get_settings()
    config_path = settings.config_file_path

    if config_path.exists():
        config = read_json(config_path, default={})
        if isinstance(config, dict) and "request_backend" in config:
            backend_config = config["request_backend"]
            return RequestBackendSettings(**backend_config)

    # Default: native backend
    return RequestBackendSettings()


@singleton
def get_request_backend_service() -> "RequestBackendService":
    """Create the request backend service instance."""
    from services.request_backend_service import RequestBackendService

    # Import the DownloadService provider to avoid circular imports
    from .service_providers import get_download_service

    download_service = get_download_service()
    settings = get_request_backend_settings()

    return RequestBackendService(
        download_service=download_service,
        settings=settings,
    )