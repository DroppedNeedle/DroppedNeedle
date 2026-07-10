"""``PluginManager`` - discovery, validation, quarantine, settings and registry.

Discovery order (first registration of an id wins; later duplicates are
quarantined):

1. Built-ins: every subpackage of ``plugins/builtin``.
2. External dir: ``{ROOT_APP_DIR}/plugins`` - single ``.py`` modules and
   packages (a directory with ``__init__.py``).
3. Installed distributions exposing the ``droppedneedle.plugins`` entry-point
   group (each entry point resolves to an ``AcquisitionPlugin`` subclass).

A plugin that raises on import/instantiate/configure, declares an incompatible
``api_version``, or collides with an already-registered id is QUARANTINED: it
appears in the registry with ``loaded=False`` and the error message, and the app
keeps running. Discovery itself never raises.

Per-plugin settings and the enable flag persist through the existing
``PreferencesService`` config file under the ``plugins.{id}`` namespace
(``secret`` fields Fernet-encrypted at rest, masked in API responses). Built-in
plugins proxy both to their pre-existing settings sections via the
``enabled_override / apply_enabled / settings_values / apply_settings`` hooks,
so the legacy download-client routes and the plugin API stay in lockstep.
"""

from __future__ import annotations

import importlib
import importlib.metadata
import importlib.util
import inspect
import logging
import pkgutil
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from infrastructure.crypto import decrypt, encrypt
from plugins.base import (
    API_VERSION,
    PLUGIN_SECRET_MASK,
    AcquisitionPlugin,
    SettingsField,
    TestResult,
)

logger = logging.getLogger(__name__)

ENTRY_POINT_GROUP = "droppedneedle.plugins"
_CONFIG_KEY = "plugins"


@dataclass
class PluginRecord:
    """One discovered plugin - loaded or quarantined."""

    id: str
    name: str = ""
    version: str = ""
    source: str = "external"  # "builtin" | "external" | "entry_point"
    plugin: AcquisitionPlugin | None = None
    error: str | None = None
    module: str = ""

    @property
    def builtin(self) -> bool:
        return self.source == "builtin"

    @property
    def loaded(self) -> bool:
        return self.plugin is not None and self.error is None


@dataclass
class _Discovered:
    """A candidate plugin class plus where it came from."""

    cls: type
    source: str
    module: str
    fallback_id: str = ""
    errors: list[str] = field(default_factory=list)


class PluginManager:
    def __init__(self, preferences, external_dir: Path | None = None) -> None:
        self._preferences = preferences
        self._external_dir = external_dir
        self._records: dict[str, PluginRecord] = {}
        self._loaded = False

    # ------------------------------------------------------------------ loading

    def load(self) -> None:
        """Discover and instantiate every plugin. Idempotent; never raises."""
        if self._loaded:
            return
        self._loaded = True
        for discovered in self._discover_builtin():
            self._register(discovered)
        for discovered in self._discover_external():
            self._register(discovered)
        for discovered in self._discover_entry_points():
            self._register(discovered)
        loaded = [r.id for r in self._records.values() if r.loaded]
        broken = [r.id for r in self._records.values() if not r.loaded]
        logger.info(
            "plugins.loaded",
            extra={"loaded": ",".join(loaded), "quarantined": ",".join(broken) or "-"},
        )

    def _register(self, discovered: _Discovered) -> None:
        cls = discovered.cls
        plugin_id = str(getattr(cls, "id", "") or discovered.fallback_id)
        record = PluginRecord(
            id=plugin_id or f"unknown:{discovered.module}",
            name=str(getattr(cls, "name", "") or plugin_id),
            version=str(getattr(cls, "version", "")),
            source=discovered.source,
            module=discovered.module,
        )
        if discovered.errors:
            record.error = "; ".join(discovered.errors)
        elif not plugin_id:
            record.error = "plugin class declares no 'id'"
        elif plugin_id in self._records:
            record.error = (
                f"duplicate plugin id {plugin_id!r} (already provided by "
                f"{self._records[plugin_id].module or self._records[plugin_id].source})"
            )
            # keep the FIRST registration; store the duplicate under a synthetic key
            self._records[f"{plugin_id}!{discovered.module}"] = record
            logger.error(
                "plugins.duplicate_id",
                extra={"plugin": plugin_id, "plugin_module": discovered.module},
            )
            return
        else:
            declared = getattr(cls, "api_version", None)
            if declared != API_VERSION:
                record.error = (
                    f"plugin targets api_version {declared!r}; this server provides "
                    f"api_version {API_VERSION}"
                )
        if record.error is None:
            try:
                record.plugin = cls()
                record.plugin.configure(self.get_settings_values(record, raw=True))
            except Exception as exc:  # noqa: BLE001 - a bad plugin must never crash the app
                record.plugin = None
                record.error = f"failed to initialise: {exc}"
                logger.exception("plugins.init_failed", extra={"plugin": record.id})
        key = record.id
        if key in self._records:  # collision-safe: never clobber an earlier registration
            key = f"{key}!{discovered.module}"
        self._records[key] = record
        if record.error:
            logger.error(
                "plugins.quarantined", extra={"plugin": record.id, "error": record.error}
            )

    def _discover_builtin(self) -> list[_Discovered]:
        out: list[_Discovered] = []
        try:
            import plugins.builtin as builtin_pkg
        except Exception as exc:  # noqa: BLE001
            logger.exception("plugins.builtin_package_broken: %s", exc)
            return out
        for info in pkgutil.iter_modules(builtin_pkg.__path__):
            module_name = f"plugins.builtin.{info.name}"
            try:
                module = importlib.import_module(module_name)
            except Exception as exc:  # noqa: BLE001
                out.append(
                    _Discovered(
                        cls=_BrokenPlugin,
                        source="builtin",
                        module=module_name,
                        fallback_id=info.name,
                        errors=[f"import failed: {exc}"],
                    )
                )
                logger.exception(
                    "plugins.builtin_import_failed", extra={"plugin_module": module_name}
                )
                continue
            for cls in self._plugin_classes(module):
                out.append(_Discovered(cls=cls, source="builtin", module=module_name))
        return out

    def _discover_external(self) -> list[_Discovered]:
        out: list[_Discovered] = []
        root = self._external_dir
        if root is None or not root.is_dir():
            return out
        entries = sorted(root.iterdir(), key=lambda p: p.name.lower())
        for entry in entries:
            if entry.name.startswith(("_", ".")):
                continue
            is_module = entry.is_file() and entry.suffix == ".py"
            is_package = entry.is_dir() and (entry / "__init__.py").is_file()
            if not (is_module or is_package):
                continue
            stem = entry.stem if is_module else entry.name
            module_name = f"droppedneedle_external_plugins.{stem}"
            target = entry if is_module else entry / "__init__.py"
            try:
                spec = importlib.util.spec_from_file_location(
                    module_name,
                    target,
                    submodule_search_locations=[str(entry)] if is_package else None,
                )
                if spec is None or spec.loader is None:
                    raise ImportError(f"cannot build import spec for {entry}")
                module = importlib.util.module_from_spec(spec)
                # register before exec so package-relative imports resolve
                import sys

                sys.modules[module_name] = module
                spec.loader.exec_module(module)
            except Exception as exc:  # noqa: BLE001
                out.append(
                    _Discovered(
                        cls=_BrokenPlugin,
                        source="external",
                        module=str(entry),
                        fallback_id=stem,
                        errors=[f"import failed: {exc}"],
                    )
                )
                logger.exception("plugins.external_import_failed", extra={"path": str(entry)})
                continue
            classes = self._plugin_classes(module)
            if not classes:
                out.append(
                    _Discovered(
                        cls=_BrokenPlugin,
                        source="external",
                        module=str(entry),
                        fallback_id=stem,
                        errors=["no AcquisitionPlugin subclass found in module"],
                    )
                )
                continue
            for cls in classes:
                out.append(_Discovered(cls=cls, source="external", module=str(entry)))
        return out

    def _discover_entry_points(self) -> list[_Discovered]:
        out: list[_Discovered] = []
        try:
            eps = importlib.metadata.entry_points(group=ENTRY_POINT_GROUP)
        except Exception as exc:  # noqa: BLE001
            logger.warning("plugins.entry_point_scan_failed: %s", exc)
            return out
        for ep in eps:
            try:
                cls = ep.load()
            except Exception as exc:  # noqa: BLE001
                out.append(
                    _Discovered(
                        cls=_BrokenPlugin,
                        source="entry_point",
                        module=ep.value,
                        fallback_id=ep.name,
                        errors=[f"entry point load failed: {exc}"],
                    )
                )
                logger.exception("plugins.entry_point_load_failed", extra={"entry_point": ep.name})
                continue
            if not (inspect.isclass(cls) and issubclass(cls, AcquisitionPlugin)):
                out.append(
                    _Discovered(
                        cls=_BrokenPlugin,
                        source="entry_point",
                        module=ep.value,
                        fallback_id=ep.name,
                        errors=["entry point does not resolve to an AcquisitionPlugin subclass"],
                    )
                )
                continue
            out.append(_Discovered(cls=cls, source="entry_point", module=ep.value))
        return out

    @staticmethod
    def _plugin_classes(module) -> list[type]:
        """Concrete ``AcquisitionPlugin`` subclasses DEFINED in *module* (or, for a
        package, exported by it - re-exports from the package's own submodules count)."""
        found: list[type] = []
        for _name, obj in vars(module).items():
            if (
                inspect.isclass(obj)
                and issubclass(obj, AcquisitionPlugin)
                and obj is not AcquisitionPlugin
                and not inspect.isabstract(obj)
                and obj.__module__.startswith(module.__name__.split(".")[0])
            ):
                if obj not in found:
                    found.append(obj)
        return found

    # ----------------------------------------------------------------- registry

    def list_records(self) -> list[PluginRecord]:
        self.load()
        return sorted(self._records.values(), key=lambda r: (not r.builtin, r.id))

    def get_record(self, plugin_id: str) -> PluginRecord | None:
        self.load()
        return self._records.get(plugin_id)

    def get_plugin(self, plugin_id: str) -> AcquisitionPlugin | None:
        """The loaded plugin instance for *plugin_id* regardless of the enable
        toggle (enablement is enforced by the orchestrator's live flags, exactly
        as before the plugin layer existed). ``None`` when unknown/quarantined."""
        record = self.get_record(plugin_id)
        return record.plugin if record is not None and record.loaded else None

    # --------------------------------------------------------------- enablement

    def is_enabled(self, plugin_id: str) -> bool:
        record = self.get_record(plugin_id)
        if record is None or not record.loaded:
            return False
        override = record.plugin.enabled_override()
        if override is not None:
            return override
        stored = self._plugin_config(plugin_id).get("enabled")
        return bool(stored) if stored is not None else True

    def set_enabled(self, plugin_id: str, enabled: bool) -> None:
        record = self.get_record(plugin_id)
        if record is None:
            raise KeyError(plugin_id)
        if record.loaded and record.plugin.apply_enabled(enabled):
            return
        cfg = self._plugin_config(plugin_id)
        cfg["enabled"] = enabled
        self._save_plugin_config(plugin_id, cfg)

    # ----------------------------------------------------------------- settings

    def get_settings_schema(self, plugin_id: str) -> list[SettingsField]:
        record = self.get_record(plugin_id)
        if record is None or not record.loaded:
            return []
        try:
            return record.plugin.settings_schema()
        except Exception:  # noqa: BLE001
            logger.exception("plugins.schema_failed", extra={"plugin": plugin_id})
            return []

    def get_settings_values(
        self, record_or_id: PluginRecord | str, *, raw: bool = False
    ) -> dict[str, Any]:
        """Current settings values. ``raw=True`` returns secrets decrypted (for
        ``configure``); ``raw=False`` masks non-empty secrets (for API responses)."""
        record = (
            record_or_id
            if isinstance(record_or_id, PluginRecord)
            else self.get_record(record_or_id)
        )
        if record is None:
            return {}
        schema = []
        values: dict[str, Any] | None = None
        if record.plugin is not None:
            try:
                schema = record.plugin.settings_schema()
                values = record.plugin.settings_values()
            except Exception:  # noqa: BLE001
                logger.exception("plugins.values_failed", extra={"plugin": record.id})
        plugin_owned = values is not None
        if values is None:
            stored = dict(self._plugin_config(record.id).get("settings", {}))
            values = stored
        out: dict[str, Any] = {}
        for descriptor in schema:
            value = values.get(descriptor.key, descriptor.default)
            if descriptor.type == "secret" and isinstance(value, str) and value:
                if plugin_owned:
                    # plugin returned the secret in the clear
                    out[descriptor.key] = value if raw else PLUGIN_SECRET_MASK
                else:
                    # manager-stored: encrypted at rest
                    out[descriptor.key] = (
                        decrypt(value)[0] if raw else PLUGIN_SECRET_MASK
                    )
            else:
                out[descriptor.key] = value
        return out

    def save_settings(self, plugin_id: str, values: dict[str, Any]) -> None:
        """Persist and apply a settings mapping. Masked secret sentinels resolve
        to the currently stored value; unknown keys are dropped."""
        record = self.get_record(plugin_id)
        if record is None or not record.loaded:
            raise KeyError(plugin_id)
        resolved = self._resolve_values(record, values)
        if not record.plugin.apply_settings(resolved):
            stored: dict[str, Any] = {}
            for descriptor in record.plugin.settings_schema():
                value = resolved.get(descriptor.key)
                if descriptor.type == "secret" and isinstance(value, str) and value:
                    stored[descriptor.key] = encrypt(value)
                else:
                    stored[descriptor.key] = value
            cfg = self._plugin_config(plugin_id)
            cfg["settings"] = stored
            self._save_plugin_config(plugin_id, cfg)
        record.plugin.configure(resolved)

    def _resolve_values(self, record: PluginRecord, values: dict[str, Any]) -> dict[str, Any]:
        """Coerce a submitted mapping onto the schema: keep known keys, fill
        missing ones from current values, and swap masked secrets for the stored
        plaintext."""
        current = self.get_settings_values(record, raw=True)
        resolved: dict[str, Any] = dict(current)
        schema = {d.key: d for d in record.plugin.settings_schema()}
        for key, value in values.items():
            descriptor = schema.get(key)
            if descriptor is None:
                continue
            if (
                descriptor.type == "secret"
                and isinstance(value, str)
                and value == PLUGIN_SECRET_MASK
            ):
                continue  # keep current
            if isinstance(value, str):
                value = value.strip() if descriptor.type in ("secret", "str", "select") else value
            resolved[key] = value
        return resolved

    async def test(self, plugin_id: str, values: dict[str, Any] | None = None) -> TestResult:
        """Run the plugin's connection test - against *values* (masked secrets
        resolved, nothing persisted) or the current configuration."""
        record = self.get_record(plugin_id)
        if record is None:
            return TestResult(ok=False, message=f"unknown plugin {plugin_id!r}")
        if not record.loaded:
            return TestResult(ok=False, message=record.error or "plugin failed to load")
        try:
            resolved = self._resolve_values(record, values) if values else None
            return await record.plugin.test_connection(resolved)
        except Exception as exc:  # noqa: BLE001 - a broken test must not 500
            logger.exception("plugins.test_failed", extra={"plugin": plugin_id})
            return TestResult(ok=False, message=f"test failed: {exc}")

    async def shutdown_all(self) -> None:
        for record in self._records.values():
            if record.loaded:
                try:
                    await record.plugin.shutdown()
                except Exception:  # noqa: BLE001
                    logger.exception("plugins.shutdown_failed", extra={"plugin": record.id})

    # -------------------------------------------------------------- persistence

    def _plugin_config(self, plugin_id: str) -> dict[str, Any]:
        section = self._preferences.get_plugins_config()
        return dict(section.get(plugin_id, {}))

    def _save_plugin_config(self, plugin_id: str, cfg: dict[str, Any]) -> None:
        # raw variant: acquisition settings are typed and pre-encrypted here, so
        # they must bypass the manifest host's str-coercing save_plugin_config
        self._preferences.save_plugin_config_raw(plugin_id, cfg)


class _BrokenPlugin(AcquisitionPlugin):
    """Placeholder class for discovery failures - registered so a broken module
    still shows up (quarantined) in the admin registry. ``_register`` sees the
    empty id / errors and never instantiates it as a working plugin."""

    id = ""
    name = ""
    version = ""
    api_version = -1  # guarantees quarantine even if something tries to load it

    def settings_schema(self):  # pragma: no cover - never called
        return []

    def configure(self, settings):  # pragma: no cover - never called
        return None

    async def test_connection(self, settings=None):  # pragma: no cover - never called
        return TestResult(ok=False, message="broken plugin")

    async def search(self, request):  # pragma: no cover - never called
        return []

    async def enqueue(self, request):  # pragma: no cover - never called
        raise NotImplementedError

    async def get_status(self, handle):  # pragma: no cover - never called
        raise NotImplementedError

    async def cancel(self, handle):  # pragma: no cover - never called
        return False

    async def completed_path(self, handle):  # pragma: no cover - never called
        return []
