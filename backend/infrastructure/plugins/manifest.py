"""Plugin manifest: ``plugin.toml`` at a plugin package's root.

The manifest is the contract's front door: the host refuses to import any code
before the manifest parses, declares a compatible ``api_version``, and names
only known capabilities. Capability ids are deliberately generic - the API
carries no acquisition examples anywhere (D22).
"""

import tomllib
from pathlib import Path

from infrastructure.msgspec_fastapi import AppStruct

PLUGIN_API_VERSION = 0  # EXPERIMENTAL: surface may change until api_version 1

# 'metadata_provider' and 'streaming_source' are accepted (reserved) but not
# activated yet - the host logs and skips them until a future api_version wires
# their consumers. Validating them now keeps early manifests forward-compatible.
ACTIVE_CAPABILITIES = frozenset({"scrobbler", "purchase_links", "audio_source"})
RESERVED_CAPABILITIES = frozenset({"metadata_provider", "streaming_source"})
KNOWN_CAPABILITIES = ACTIVE_CAPABILITIES | RESERVED_CAPABILITIES


class ManifestError(Exception):
    """The manifest is missing, unparsable, or declares an invalid contract."""


class PluginSettingField(AppStruct):
    """One admin-editable setting the plugin wants (rendered by the generic
    settings UI; values are stored per-plugin in config.json)."""

    key: str
    label: str
    help: str = ""
    secret: bool = False


class PluginManifest(AppStruct):
    name: str  # unique id, kebab-case
    version: str
    api_version: int
    entrypoint: str  # "<module>:<ClassName>" inside the plugin package
    capabilities: list[str]
    display_name: str = ""
    description: str = ""
    author: str = ""
    homepage: str = ""
    settings: list[PluginSettingField] = []


def load_manifest(plugin_dir: Path) -> PluginManifest:
    path = plugin_dir / "plugin.toml"
    if not path.is_file():
        raise ManifestError(f"{plugin_dir.name}: no plugin.toml")
    try:
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
    except (tomllib.TOMLDecodeError, OSError, UnicodeDecodeError) as exc:
        raise ManifestError(f"{plugin_dir.name}: unreadable plugin.toml ({exc})") from exc

    plugin = raw.get("plugin")
    if not isinstance(plugin, dict):
        raise ManifestError(f"{plugin_dir.name}: missing [plugin] table")

    name = str(plugin.get("name") or "").strip()
    if not name or not all(c.isalnum() or c in "-_" for c in name):
        raise ManifestError(f"{plugin_dir.name}: invalid plugin name {name!r}")

    try:
        api_version = int(plugin.get("api_version"))
    except (TypeError, ValueError):
        raise ManifestError(f"{name}: api_version must be an integer") from None
    if api_version != PLUGIN_API_VERSION:
        raise ManifestError(
            f"{name}: api_version {api_version} unsupported (host speaks {PLUGIN_API_VERSION})"
        )

    entrypoint = str(plugin.get("entrypoint") or "").strip()
    if ":" not in entrypoint:
        raise ManifestError(f"{name}: entrypoint must be '<module>:<ClassName>'")

    capabilities = plugin.get("capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        raise ManifestError(f"{name}: at least one capability is required")
    unknown = [c for c in capabilities if c not in KNOWN_CAPABILITIES]
    if unknown:
        raise ManifestError(f"{name}: unknown capabilities {unknown}")

    fields: list[PluginSettingField] = []
    for entry in raw.get("settings", []) or []:
        if not isinstance(entry, dict) or not entry.get("key"):
            raise ManifestError(f"{name}: each [[settings]] entry needs a key")
        fields.append(
            PluginSettingField(
                key=str(entry["key"]),
                label=str(entry.get("label") or entry["key"]),
                help=str(entry.get("help") or ""),
                secret=bool(entry.get("secret", False)),
            )
        )

    return PluginManifest(
        name=name,
        version=str(plugin.get("version") or "0.0.0"),
        api_version=api_version,
        entrypoint=entrypoint,
        capabilities=[str(c) for c in capabilities],
        display_name=str(plugin.get("display_name") or name),
        description=str(plugin.get("description") or ""),
        author=str(plugin.get("author") or ""),
        homepage=str(plugin.get("homepage") or ""),
        settings=fields,
    )
