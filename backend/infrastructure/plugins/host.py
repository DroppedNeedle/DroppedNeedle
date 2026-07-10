"""PluginHost - discover, validate, and run plugins from the plugins directory.

Trust model (documented, not enforced): a plugin is Python imported in-process
with the app's full privileges. That is why nothing loads until BOTH the
manifest validates AND the admin has explicitly enabled the plugin in
Settings -> Plugins. Dropping a folder in the directory alone runs no code.

Failure isolation: a plugin that fails to import, instantiate, or execute is
recorded (and surfaced in the admin UI) - it never crashes the host or the
flow that invoked it.
"""

import asyncio
import importlib.util
import io
import logging
import re
import shutil
import sys
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING

from infrastructure.msgspec_fastapi import AppStruct
from infrastructure.plugins.manifest import (
    ManifestError,
    PluginManifest,
    load_manifest,
)
from infrastructure.plugins.protocols import (
    CAPABILITY_PROTOCOLS,
    PluginContext,
    PluginPurchaseLink,
    ScrobbleEvent,
    SourceItem,
)

if TYPE_CHECKING:
    from services.preferences_service import PreferencesService

logger = logging.getLogger(__name__)

# https://github.com/<owner>/<repo>[/tree/<ref>][.git][/]
_GITHUB_URL = re.compile(
    r"^https://github\.com/(?P<owner>[\w.-]+)/(?P<repo>[\w.-]+?)(?:\.git)?"
    r"(?:/tree/(?P<ref>[\w./-]+))?/?$"
)
_CODELOAD = "https://codeload.github.com/{owner}/{repo}/zip/refs/heads/{ref}"
_MAX_PLUGIN_ZIP_BYTES = 32 * 2**20
_MAX_PLUGIN_ZIP_ENTRIES = 2000


class PluginInstallError(Exception):
    """The repository is not a usable plugin (bad URL, no manifest, unsafe zip)."""


class LoadedPlugin(AppStruct):
    manifest: PluginManifest
    enabled: bool
    error: str | None = None
    active_capabilities: list[str] = []
    # the on-disk folder, which need not equal the manifest name (a hand-copied
    # plugin can live in any directory); uninstall removes exactly this one
    directory: str = ""

    instance: object | None = None


class PluginHost:
    def __init__(self, *, plugins_dir: Path, preferences_service: "PreferencesService") -> None:
        self._dir = plugins_dir
        self._prefs = preferences_service
        self._plugins: dict[str, LoadedPlugin] = {}

    # -- lifecycle --

    def load_all(self) -> None:
        """Discover every plugin folder; import only manifest-valid, admin-enabled
        ones. Runs once at startup and on explicit reload - in a worker thread,
        so the registry is built aside and swapped in atomically: a dispatch on
        the event loop never sees a half-populated dict."""
        plugins: dict[str, LoadedPlugin] = {}
        if self._dir.is_dir():
            for child in sorted(self._dir.iterdir()):
                if not child.is_dir() or child.name.startswith(("_", ".")):
                    continue
                self._load_one(child, plugins)
        self._plugins = plugins
        if plugins:
            summary = {
                name: (p.active_capabilities if p.enabled else "disabled")
                for name, p in plugins.items()
            }
            logger.info("plugins.loaded", extra={"plugins": str(summary)})

    # -- install from GitHub --

    async def install_from_github(self, url: str, http) -> str:  # noqa: ANN001 - httpx.AsyncClient
        """Download a public GitHub repo's default (or given) branch as a zip and
        unpack it into the plugins directory. Returns the installed plugin name.

        NOTE: this downloads and stores third-party code; it does NOT run it.
        The installed plugin stays disabled until an admin enables it, which is
        the same trust gate as a hand-copied folder.
        """
        match = _GITHUB_URL.match((url or "").strip())
        if match is None:
            raise PluginInstallError(
                "Enter a public GitHub repository URL, e.g. https://github.com/owner/repo"
            )
        owner, repo = match["owner"], match["repo"]
        refs = [match["ref"]] if match["ref"] else ["main", "master"]
        # the character classes permit dots, so a '..' component could still walk
        # the codeload path; reject it rather than rely on URL normalisation
        if any(".." in part for part in (owner, repo, *refs)):
            raise PluginInstallError("That repository URL is not valid")

        archive: bytes | None = None
        for ref in refs:
            response = await http.get(
                _CODELOAD.format(owner=owner, repo=repo, ref=ref), follow_redirects=True
            )
            if response.status_code != 200:
                continue
            declared = response.headers.get("content-length")
            if declared and declared.isdigit() and int(declared) > _MAX_PLUGIN_ZIP_BYTES:
                raise PluginInstallError("That repository is too large to install as a plugin")
            archive = response.content
            break
        if archive is None:
            raise PluginInstallError(
                "Could not download that repository - check the URL is public and the branch exists"
            )
        if len(archive) > _MAX_PLUGIN_ZIP_BYTES:
            raise PluginInstallError("That repository is too large to install as a plugin")

        name = await asyncio.to_thread(self._unpack_plugin_zip, archive)
        await asyncio.to_thread(self.load_all)
        logger.info("plugins.installed name=%s source=%s", name, url)
        return name

    def _unpack_plugin_zip(self, archive: bytes) -> str:
        """Extract the archive's single top-level dir into the plugins folder,
        named after the manifest. Refuses traversal, absolute paths, symlinks,
        and anything without a valid plugin.toml."""
        with zipfile.ZipFile(io.BytesIO(archive)) as zf:
            entries = [e for e in zf.infolist() if not e.is_dir()]
            if len(entries) > _MAX_PLUGIN_ZIP_ENTRIES:
                raise PluginInstallError("That repository has too many files")
            roots = {Path(e.filename).parts[0] for e in entries if Path(e.filename).parts}
            if len(roots) != 1:
                raise PluginInstallError("Unexpected archive layout")
            root = roots.pop()

            manifest_entry = next(
                (e for e in entries if Path(e.filename).parts[1:] == ("plugin.toml",)), None
            )
            if manifest_entry is None:
                raise PluginInstallError(
                    "No plugin.toml at the repository root - this is not a DroppedNeedle plugin"
                )

            staging = self._dir / f".installing-{root}"
            shutil.rmtree(staging, ignore_errors=True)
            try:
                for entry in entries:
                    parts = Path(entry.filename).parts[1:]  # strip the repo-ref root
                    if not parts:
                        continue
                    raw = Path(*parts)
                    if raw.is_absolute() or ".." in raw.parts:
                        raise PluginInstallError("The archive contains unsafe paths")
                    # 0xA000 = symlink in the zip's external attrs (unix mode)
                    if (entry.external_attr >> 16) & 0xF000 == 0xA000:
                        raise PluginInstallError("The archive contains symlinks")
                    target = staging.joinpath(raw)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with zf.open(entry) as src, open(target, "wb") as dst:
                        shutil.copyfileobj(src, dst)

                manifest = load_manifest(staging)  # validates before it can be enabled
                final = self._dir / manifest.name
                shutil.rmtree(final, ignore_errors=True)
                staging.replace(final)
                return manifest.name
            except ManifestError as exc:
                raise PluginInstallError(str(exc)) from exc
            finally:
                shutil.rmtree(staging, ignore_errors=True)

    def uninstall(self, plugin_name: str) -> None:
        """Disable-by-deletion: remove the plugin's folder. The admin's saved
        settings stay in config.json, so a reinstall keeps its configuration."""
        plugin = self._plugins.get(plugin_name)
        if plugin is None:
            from core.exceptions import ResourceNotFoundError

            raise ResourceNotFoundError("Plugin not found")
        target = Path(plugin.directory) if plugin.directory else self._dir / plugin_name
        if target.is_dir() and target.parent == self._dir:
            shutil.rmtree(target, ignore_errors=True)
        self.load_all()

    def _load_one(self, plugin_dir: Path, plugins: dict[str, LoadedPlugin]) -> None:
        try:
            manifest = load_manifest(plugin_dir)
        except ManifestError as exc:
            logger.warning("plugins.manifest_invalid: %s", exc)
            plugins[plugin_dir.name] = LoadedPlugin(
                manifest=PluginManifest(
                    name=plugin_dir.name,
                    version="",
                    api_version=0,
                    entrypoint="",
                    capabilities=[],
                ),
                enabled=False,
                error=str(exc),
                directory=str(plugin_dir),
            )
            return

        enabled = self._prefs.get_plugin_config(manifest.name).enabled
        plugin = LoadedPlugin(manifest=manifest, enabled=enabled, directory=str(plugin_dir))
        plugins[manifest.name] = plugin
        if not enabled:
            return

        try:
            instance = self._instantiate(plugin_dir, manifest)
        except Exception as exc:  # noqa: BLE001 - a broken plugin must never crash the host
            logger.error("plugins.load_failed name=%s: %s", manifest.name, exc)
            plugin.error = f"Failed to load: {exc}"
            plugin.enabled = False
            return

        active: list[str] = []
        for capability in manifest.capabilities:
            protocol = CAPABILITY_PROTOCOLS.get(capability)
            if protocol is None:
                logger.info(
                    "plugins.capability_reserved name=%s capability=%s (not active yet)",
                    manifest.name,
                    capability,
                )
                continue
            if isinstance(instance, protocol):
                active.append(capability)
            else:
                logger.warning(
                    "plugins.capability_unimplemented name=%s capability=%s",
                    manifest.name,
                    capability,
                )
        plugin.instance = instance
        plugin.active_capabilities = active

    def _instantiate(self, plugin_dir: Path, manifest: PluginManifest) -> object:
        module_name, _, class_name = manifest.entrypoint.partition(":")
        module_path = plugin_dir / f"{module_name}.py"
        if not module_path.is_file():
            raise ManifestError(f"entrypoint module {module_name}.py not found")
        # namespaced so two plugins may both ship e.g. plugin.py
        full_name = f"droppedneedle_plugin.{manifest.name}.{module_name}"
        spec = importlib.util.spec_from_file_location(full_name, module_path)
        if spec is None or spec.loader is None:
            raise ManifestError("entrypoint module could not be prepared")
        module = importlib.util.module_from_spec(spec)
        sys.modules[full_name] = module
        spec.loader.exec_module(module)
        entry_cls = getattr(module, class_name, None)
        if entry_cls is None:
            raise ManifestError(f"entrypoint class {class_name} not found")
        context = PluginContext(
            plugin_name=manifest.name,
            settings=self._settings_getter(manifest.name),
            http=self._plugin_http_client(manifest.name),
        )
        return entry_cls(context)

    def _settings_getter(self, name: str):
        def _get() -> dict[str, str]:
            return self._prefs.get_plugin_config(name).settings

        return _get

    @staticmethod
    def _plugin_http_client(name: str):
        from infrastructure.http.client import HttpClientFactory

        # one named client per plugin: the factory caches by name and the
        # first caller's kwargs win, so plugins never share timeout surprises
        return HttpClientFactory.get_client(name=f"plugin-{name}", timeout=30.0)

    # -- queries --

    def list_plugins(self) -> list[LoadedPlugin]:
        return list(self._plugins.values())

    def get(self, name: str) -> LoadedPlugin | None:
        return self._plugins.get(name)

    def _active(self, capability: str) -> list[LoadedPlugin]:
        return [
            p
            for p in self._plugins.values()
            if p.enabled and p.instance is not None and capability in p.active_capabilities
        ]

    def sources(self) -> list[LoadedPlugin]:
        return self._active("audio_source")

    def purchase_providers(self) -> list[LoadedPlugin]:
        return self._active("purchase_links")

    # -- capability dispatch (all best-effort, per-plugin isolation) --

    async def dispatch_scrobble(self, event: ScrobbleEvent) -> None:
        for plugin in self._active("scrobbler"):
            try:
                await plugin.instance.on_scrobble(event)  # type: ignore[union-attr]
            except Exception as exc:  # noqa: BLE001 - plugin errors never break scrobbling
                logger.warning(
                    "plugins.scrobble_failed name=%s: %s", plugin.manifest.name, exc
                )

    async def gather_purchase_links(
        self, artist: str, album: str, release_group_mbid: str
    ) -> list[PluginPurchaseLink]:
        links: list[PluginPurchaseLink] = []
        for plugin in self._active("purchase_links"):
            try:
                async with asyncio.timeout(10):
                    links.extend(
                        await plugin.instance.purchase_links(  # type: ignore[union-attr]
                            artist, album, release_group_mbid
                        )
                    )
            except Exception as exc:  # noqa: BLE001 - plugin errors never break Get-it
                logger.warning(
                    "plugins.purchase_links_failed name=%s: %s", plugin.manifest.name, exc
                )
        return links

    async def source_search(self, plugin_name: str, query: str, limit: int = 20) -> list[SourceItem]:
        plugin = self.require_source(plugin_name)
        return await plugin.instance.search(query, limit)  # type: ignore[union-attr]

    async def source_fetch(self, plugin_name: str, item_id: str, dest_dir: Path) -> list[Path]:
        plugin = self.require_source(plugin_name)
        dest_dir.mkdir(parents=True, exist_ok=True)
        return await plugin.instance.fetch(item_id, dest_dir)  # type: ignore[union-attr]

    def require_source(self, plugin_name: str) -> LoadedPlugin:
        from core.exceptions import ResourceNotFoundError

        plugin = self._plugins.get(plugin_name)
        if (
            plugin is None
            or not plugin.enabled
            or plugin.instance is None
            or "audio_source" not in plugin.active_capabilities
        ):
            raise ResourceNotFoundError(f"No enabled source plugin named {plugin_name}")
        return plugin
