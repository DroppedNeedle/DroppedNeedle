"""Admin plugin-registry API schemas."""

from typing import Any

from infrastructure.msgspec_fastapi import AppStruct
from plugins.base import SettingsField


class PluginInfo(AppStruct):
    id: str
    name: str
    version: str
    builtin: bool
    enabled: bool
    loaded: bool
    error: str | None = None
    source: str = "external"
    api_version: int | None = None


class PluginListResponse(AppStruct):
    plugins: list[PluginInfo] = []
    api_version: int = 0


class PluginToggleResponse(AppStruct):
    id: str
    enabled: bool


class PluginSettingsResponse(AppStruct):
    """Schema + current values (secrets masked)."""

    id: str
    schema: list[SettingsField] = []
    values: dict[str, Any] = {}


class PluginSettingsUpdate(AppStruct):
    values: dict[str, Any] = {}


class PluginTestRequest(AppStruct):
    """Optional values to test WITHOUT saving (masked secrets resolve to stored)."""

    values: dict[str, Any] = {}


class PluginTestResponse(AppStruct):
    valid: bool
    message: str = ""
    version: str | None = None
