"""DI provider for the acquisition ``PluginManager``.

One process-wide manager: discovery (built-ins + ``{ROOT_APP_DIR}/plugins`` +
the ``droppedneedle.plugins`` entry-point group) runs once on first access and
never raises - a broken plugin is quarantined in the registry instead.

The manager singleton survives download-settings saves on purpose: built-in
plugins resolve their client singletons lazily per call, so the existing
cache-clear chains rebuild what matters without reloading plugin modules.
"""

from __future__ import annotations

from ._registry import singleton
from .cache_providers import get_preferences_service


@singleton
def get_plugin_manager() -> "PluginManager":
    from core.config import get_settings
    from plugins.manager import PluginManager

    manager = PluginManager(
        preferences=get_preferences_service(),
        external_dir=get_settings().root_app_dir / "plugins",
    )
    manager.load()
    return manager
