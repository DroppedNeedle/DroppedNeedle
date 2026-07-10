"""``AcquisitionPlugin`` - the versioned contract every acquisition plugin implements.

The plugin seam is ACQUISITION only: search for release candidates, enqueue a
download with the external client, report progress, report where the finished
files landed, cancel. Everything after that (identification, move into the
library, registration) is the core import pipeline and is not pluggable.

The exchange types are NOT invented for the plugin layer - they are the exact
structs the download orchestrator already exchanges with the slskd and SABnzbd
clients (``repositories/protocols``), re-exported here, so wrapping the two
built-in clients as plugins is lossless:

- ``Candidate``          = ``IndexerResult``      (one search hit, tagged by source)
- ``EnqueueRequest``     -> ``TaskHandle``        (start a download, get a correlation handle)
- ``TaskHandle``         -> ``DownloadTaskStatus``(poll progress)
- ``completed_path``     -> ``list[Path]``        (where the finished audio files are)

This module deliberately does NOT use ``from __future__ import annotations``:
the protocol conformance contract tests compare real ``inspect.signature``
objects (including return annotations) between ``IndexerProtocol`` /
``DownloadClientProtocol`` and the adapters defined at the bottom of this file,
so annotations must be real objects and byte-identical to the protocols'.
"""

import logging
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Any, ClassVar, Literal

from infrastructure.msgspec_fastapi import AppStruct
from models.common import ServiceStatus
from repositories.protocols.download_client import (  # noqa: F401 - re-exported plugin API
    DownloadFileRef,
    DownloadSearchResult,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    TaskHandle,
)
from repositories.protocols.indexer import (  # noqa: F401 - re-exported plugin API
    IndexerResult,
    UsenetRelease,
)

logger = logging.getLogger(__name__)

# The plugin API version this server implements. A plugin declares the version it
# was written against via ``AcquisitionPlugin.api_version``; the manager refuses to
# load a plugin whose declared version differs (see docs/plugins/AUTHORING.md for
# the compatibility policy).
API_VERSION = 1

# Generic masked-secret sentinel for plugin settings values. A ``secret`` field
# whose stored value is non-empty is returned as this mask; saving the mask back
# preserves the stored value (the house pattern used by every settings section).
PLUGIN_SECRET_MASK = "plugin****"

# One search hit. Exactly the struct the orchestrator's scorers consume today: a
# tagged union whose ``source`` names the plugin/source id, carrying either a
# per-file Soulseek-shaped result (``soulseek``) or a release-shaped result
# (``usenet``). Third-party plugins emit whichever archetype fits their network:
# per-file peer-to-peer results or single-archive release results.
Candidate = IndexerResult

FieldType = Literal["str", "int", "bool", "select", "secret"]


class SelectOption(AppStruct):
    """One choice of a ``select`` field. ``value`` is what gets stored."""

    value: str | int
    label: str = ""


class SettingsField(AppStruct):
    """One typed field of a plugin's settings schema.

    The admin UI renders the form from these descriptors; the manager uses
    ``type == "secret"`` to encrypt at rest and mask in API responses.
    """

    key: str
    type: FieldType = "str"
    label: str = ""
    help: str = ""
    default: str | int | float | bool | None = None
    required: bool = False
    options: list[SelectOption] = []


class TestResult(AppStruct):
    """Outcome of ``test_connection``. ``ok=False`` carries a human-readable
    ``message`` the admin UI shows verbatim."""

    ok: bool
    message: str = ""
    version: str | None = None


class SearchRequest(AppStruct):
    """One search the orchestrator asks a plugin to run.

    ``kind`` selects the shape: an ``album`` search carries ``album_title`` /
    ``track_count``; a ``track`` search carries ``track_title`` /
    ``duration_seconds`` (and optionally ``album_title`` for context).
    """

    kind: Literal["album", "track"] = "album"
    artist_name: str = ""
    album_title: str | None = None
    track_title: str | None = None
    year: int | None = None
    track_count: int | None = None
    duration_seconds: int | None = None
    timeout: float = 30.0


class AcquisitionPlugin(ABC):
    """Base class for acquisition plugins.

    Identity is declared as class attributes (``id``, ``name``, ``version``,
    ``api_version``). The lifecycle is: instantiate -> ``configure(settings)``
    -> serve ``search`` / ``enqueue`` / ``get_status`` / ``cancel`` /
    ``completed_path`` -> ``shutdown()`` on app exit.

    A plugin instance is a process-wide singleton owned by the ``PluginManager``.
    Methods may be called concurrently from the event loop; do not block.
    """

    # -- identity (override all three; api_version pins the contract version) --
    id: ClassVar[str] = ""
    name: ClassVar[str] = ""
    version: ClassVar[str] = "0.0.0"
    api_version: ClassVar[int] = API_VERSION

    # ---------------------------------------------------------------- lifecycle

    @abstractmethod
    def settings_schema(self) -> list[SettingsField]:
        """The typed field descriptors the admin settings form is rendered from."""

    @abstractmethod
    def configure(self, settings: dict[str, Any]) -> None:
        """Apply a settings mapping (schema keys -> values, secrets decrypted).

        Called once after load with the persisted values and again after every
        settings save. Must be cheap and must not perform I/O; validate lazily
        in ``test_connection`` / first use instead of raising here.
        """

    @abstractmethod
    async def test_connection(self, settings: dict[str, Any] | None = None) -> TestResult:
        """Verify connectivity/credentials. ``settings=None`` tests the currently
        configured values; a mapping tests those values WITHOUT persisting them
        (the admin UI tests before saving)."""

    async def shutdown(self) -> None:
        """Release resources (HTTP clients, background tasks). Best-effort;
        exceptions are logged and swallowed by the manager."""
        return None

    # -------------------------------------------------------------- capabilities

    def is_configured(self) -> bool:
        """True when the plugin has enough configuration to be used at all
        (distinct from the admin enable toggle)."""
        return True

    async def health_check(self) -> ServiceStatus:
        """Liveness of the backing service. Default: derived from
        ``test_connection()``."""
        result = await self.test_connection()
        return ServiceStatus(
            status="ok" if result.ok else "error",
            version=result.version,
            message=result.message or None,
        )

    @abstractmethod
    async def search(self, request: SearchRequest) -> list[Candidate]:
        """Run one search and return candidates tagged with this plugin's source id."""

    @abstractmethod
    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        """Start downloading and return the correlation handle the orchestrator
        persists and hands back to ``get_status`` / ``cancel`` / ``completed_path``."""

    @abstractmethod
    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        """Progress of an enqueued download."""

    @abstractmethod
    async def cancel(self, handle: TaskHandle) -> bool:
        """Cancel an in-flight download AND/OR remove its completed transfer
        records (also used for post-import cleanup)."""

    @abstractmethod
    async def completed_path(self, handle: TaskHandle) -> list[Path]:
        """On-disk paths of the finished job's audio files - the hand-off point
        to the core import pipeline."""

    async def get_file_path(
        self,
        handle: TaskHandle,
        remote_filename: str,
        size: int | None = None,
    ) -> Path | None:
        """Local path of ONE completed file, when the plugin can resolve remote
        names to disk names. Optional; ``None`` makes the orchestrator fall back
        to ``completed_path`` / ``get_status``."""
        return None

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        """Optional cross-check of the client's completed downloads against the
        configured import mount. Default: not supported."""
        return MountDiagnosis(supported=False)

    # -------------------------------------------------- orchestrator integration
    # The download orchestrator speaks IndexerProtocol + DownloadClientProtocol.
    # The default adapters below wrap this plugin's own methods; built-in plugins
    # override these to return the exact pre-existing client objects, making the
    # refactor lossless.

    def get_indexer(self):  # noqa: ANN201 - IndexerProtocol (structural)
        """This plugin's search side as an ``IndexerProtocol``."""
        return PluginIndexerAdapter(self)

    def get_download_client(self):  # noqa: ANN201 - DownloadClientProtocol (structural)
        """This plugin's acquire/track/locate side as a ``DownloadClientProtocol``."""
        return PluginDownloadClientAdapter(self)

    # ------------------------------------------------------- built-in overrides
    # Hooks that let a plugin proxy its enable flag / settings values to an
    # existing persistence location instead of the manager's generic
    # ``plugins.{id}`` config namespace. Third-party plugins never override these.

    def enabled_override(self) -> bool | None:
        """Return the live enable flag when the plugin owns it elsewhere
        (built-ins proxy the existing settings sections); ``None`` means the
        manager persists it in the plugin config namespace."""
        return None

    def apply_enabled(self, enabled: bool) -> bool:
        """Persist the enable flag when the plugin owns it elsewhere. Return
        ``True`` when handled; ``False`` lets the manager persist it."""
        return False

    def settings_values(self) -> dict[str, Any] | None:
        """Return current settings values (secrets in the clear) when the plugin
        stores them elsewhere; ``None`` means the manager owns persistence."""
        return None

    def apply_settings(self, values: dict[str, Any]) -> bool:
        """Persist a full settings mapping when the plugin stores them elsewhere.
        Return ``True`` when handled; ``False`` lets the manager persist them."""
        return False


class PluginIndexerAdapter:
    """``IndexerProtocol`` view over a plugin's ``search`` capability.

    Signatures are byte-identical to ``IndexerProtocol`` (contract-tested), so
    the orchestrator needs zero changes to consume a third-party plugin's search.
    """

    def __init__(self, plugin: AcquisitionPlugin) -> None:
        self._plugin = plugin

    @property
    def indexer_name(self) -> str:
        return self._plugin.id

    def is_configured(self) -> bool:
        return self._plugin.is_configured()

    async def health_check(self) -> ServiceStatus:
        return await self._plugin.health_check()

    async def search_album(
        self,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        return await self._plugin.search(
            SearchRequest(
                kind="album",
                artist_name=artist_name,
                album_title=album_title,
                year=year,
                track_count=track_count,
                timeout=timeout,
            )
        )

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        return await self._plugin.search(
            SearchRequest(
                kind="track",
                artist_name=artist_name,
                track_title=track_title,
                album_title=album_title,
                duration_seconds=duration_seconds,
                timeout=timeout,
            )
        )


class PluginDownloadClientAdapter:
    """``DownloadClientProtocol`` view over a plugin's acquire/track/locate
    capabilities. Signatures are byte-identical to the protocol (contract-tested)."""

    def __init__(self, plugin: AcquisitionPlugin) -> None:
        self._plugin = plugin

    @property
    def client_name(self) -> str:
        return self._plugin.id

    def is_configured(self) -> bool:
        return self._plugin.is_configured()

    async def health_check(self) -> ServiceStatus:
        return await self._plugin.health_check()

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        return await self._plugin.enqueue(request)

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        return await self._plugin.get_status(handle)

    async def cancel(self, handle: TaskHandle) -> bool:
        return await self._plugin.cancel(handle)

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        return await self._plugin.completed_path(handle)

    async def get_file_path(
        self,
        handle: TaskHandle,
        remote_filename: str,
        size: int | None = None,
    ) -> Path | None:
        return await self._plugin.get_file_path(handle, remote_filename, size)

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        return await self._plugin.diagnose_downloads_mount()
