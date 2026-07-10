"""DroppedNeedle plugin API (phase 01b, EXPERIMENTAL - api_version 0).

See PLUGINS.md at the repository root for the authored contract. The host
loads third-party plugin packages from ``<root_app_dir>/plugins``; nothing is
bundled and no registry exists (a plugin is third-party by construction).
"""

from infrastructure.plugins.host import PluginHost, LoadedPlugin
from infrastructure.plugins.manifest import PluginManifest, ManifestError
from infrastructure.plugins.protocols import (
    PluginContext,
    PluginPurchaseLink,
    ScrobbleEvent,
    SourceItem,
)

__all__ = [
    "PluginHost",
    "LoadedPlugin",
    "PluginManifest",
    "ManifestError",
    "PluginContext",
    "PluginPurchaseLink",
    "ScrobbleEvent",
    "SourceItem",
]
