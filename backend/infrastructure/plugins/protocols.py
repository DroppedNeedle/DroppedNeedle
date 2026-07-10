"""Capability protocols - what a plugin implements, and what the host hands it.

A plugin's entrypoint class is instantiated once as ``Entry(context)`` and may
implement any subset of the capability methods matching its manifest. All
methods are async and best-effort: an exception is logged against the plugin
and never propagates into the host flow that triggered it.
"""

import logging
from pathlib import Path
from typing import Callable, Mapping, Protocol, runtime_checkable

import httpx

from infrastructure.msgspec_fastapi import AppStruct


class ScrobbleEvent(AppStruct):
    """A play accepted by the scrobble pipeline (already deduped)."""

    artist: str
    track: str
    album: str | None = None
    timestamp: int = 0
    duration_ms: int | None = None
    recording_mbid: str | None = None


class PluginPurchaseLink(AppStruct):
    """A purchase link a plugin contributes to the Where-to-buy section."""

    label: str
    url: str
    kind: str = "digital"  # 'digital' | 'physical' | 'free'


class SourceItem(AppStruct):
    """One result from an audio source plugin's search."""

    id: str
    title: str
    artist: str = ""
    detail: str = ""  # free text shown under the title (date, venue, format...)


class PluginContext:
    """Host-provided services for one plugin instance.

    ``settings`` is a live callable so an admin's settings save applies without
    a reload. ``http`` is a shared factory client (owns timeouts + the app
    User-Agent); plugins must not build their own clients.
    """

    def __init__(
        self,
        *,
        plugin_name: str,
        settings: Callable[[], Mapping[str, str]],
        http: httpx.AsyncClient,
    ) -> None:
        self.plugin_name = plugin_name
        self._settings = settings
        self.http = http
        self.logger = logging.getLogger(f"plugin.{plugin_name}")

    @property
    def settings(self) -> Mapping[str, str]:
        return self._settings()


@runtime_checkable
class ScrobblerCapability(Protocol):
    async def on_scrobble(self, event: ScrobbleEvent) -> None: ...


@runtime_checkable
class PurchaseLinksCapability(Protocol):
    async def purchase_links(
        self, artist: str, album: str, release_group_mbid: str
    ) -> list[PluginPurchaseLink]: ...


@runtime_checkable
class AudioSourceCapability(Protocol):
    async def search(self, query: str, limit: int) -> list[SourceItem]: ...

    async def fetch(self, item_id: str, dest_dir: Path) -> list[Path]: ...


CAPABILITY_PROTOCOLS: dict[str, type] = {
    "scrobbler": ScrobblerCapability,
    "purchase_links": PurchaseLinksCapability,
    "audio_source": AudioSourceCapability,
}
