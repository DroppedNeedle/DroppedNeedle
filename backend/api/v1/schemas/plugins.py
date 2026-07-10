"""Wire schemas for the plugin API surfaces (phase 01b).

Secret-marked plugin settings follow the house mask-sentinel pattern: reads
return the mask when a value exists, and a save that sends the mask back keeps
the stored value.
"""

from infrastructure.msgspec_fastapi import AppStruct
from infrastructure.plugins.protocols import SourceItem

PLUGIN_SECRET_MASK = "plugin****"


class PluginSettingFieldInfo(AppStruct):
    key: str
    label: str
    help: str = ""
    secret: bool = False


class PluginInfo(AppStruct):
    name: str
    display_name: str
    version: str
    enabled: bool
    capabilities: list[str] = []
    active_capabilities: list[str] = []
    description: str = ""
    author: str = ""
    homepage: str = ""
    error: str | None = None
    settings_fields: list[PluginSettingFieldInfo] = []
    settings_values: dict[str, str] = {}


class PluginListResponse(AppStruct):
    plugins: list[PluginInfo] = []


class PluginUpdateRequest(AppStruct):
    enabled: bool
    settings: dict[str, str] = {}


class PluginInstallRequest(AppStruct):
    repository_url: str


class SourcePluginInfo(AppStruct):
    name: str
    display_name: str
    description: str = ""


class SourceListResponse(AppStruct):
    sources: list[SourcePluginInfo] = []


class SourceSearchRequest(AppStruct):
    query: str


class SourceSearchResponse(AppStruct):
    items: list[SourceItem] = []


class SourceFetchRequest(AppStruct):
    item_id: str


class SourceFetchResponse(AppStruct):
    started: bool = True
