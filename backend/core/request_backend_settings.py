"""Request backend configuration settings.

Defines the configuration schema for routing album acquisition requests through
different backends (e.g., native slskd, external services like Lidarr). This is a
config-gated seam: the default native mode preserves existing behavior, while
backend types enable extensibility without code changes.
"""

from pydantic import BaseModel, Field
from typing import Literal


class BackendType(BaseModel):
    """Request backend type configuration.

    Args:
        backend: Backend type identifier. 'native' routes through the built-in
                 slskd acquisition pipeline. Future backends (e.g., 'lidarr')
                 can be added here.
    """
    backend: Literal["native", "lidarr"] = Field(
        default="native",
        description="Backend for album acquisition requests",
    )


class RequestBackendSettings(BaseModel):
    """Request backend settings from config.json.

    Controls how album requests are dispatched. The default native backend
    preserves existing behavior.
    """
    request_backend: BackendType = Field(
        default_factory=BackendType,
        description="Request backend configuration",
    )