"""Error-isolated adapters over plugin download-client / indexer instances.

A plugin entrypoint implements ``DownloadClientProtocol`` /
``IndexerProtocol`` (``repositories/protocols/download_client.py``,
``repositories/protocols/indexer.py``) directly; the host wraps accepted
instances in these shims before handing them to the acquisition engine.
Every method is a one-line delegation plus a ``try/except`` that logs
against the plugin and converts the failure into a shape the engine
already understands, so a raising plugin fails its own download/search
but never escapes into the poll loop, the failover loop, or manual search.
"""

import asyncio
import logging
from pathlib import Path

from models.common import ServiceStatus
from repositories.protocols.download_client import (
    DownloadMaterialization,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    TaskHandle,
)
from repositories.protocols.indexer import IndexerResult

logger = logging.getLogger(__name__)

__all__ = ["PluginClientAdapter", "PluginIndexerAdapter"]

_TOKEN_CAP = 2000


def _orchestration_error(message: str) -> Exception:
    """The engine's control-flow failure, imported lazily so this
    infrastructure module never depends on ``services`` at load time."""
    try:
        from services.native.acquisition.errors import OrchestrationError
    except Exception:  # noqa: BLE001 - fallback keeps the adapter usable in isolation
        return RuntimeError(message)
    return OrchestrationError(message)


class PluginClientAdapter:
    """``DownloadClientProtocol`` shim over one plugin instance.

    ``client_name`` is forced to the source key ``plugin:<manifest-name>``
    (the plugin's own value is ignored); ``plugin_token`` round-trips
    through an in-memory dict keyed by task id and is reattached to the
    handle before every call that carries one.
    """

    def __init__(self, plugin_name: str, instance: object) -> None:
        self._plugin_name = plugin_name
        self._source_key = f"plugin:{plugin_name}"
        self._instance: object = instance
        self._tokens: dict[str, str] = {}

    @property
    def client_name(self) -> str:
        return self._source_key

    def is_configured(self) -> bool:
        try:
            return bool(self._instance.is_configured())  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - absence, not failure
            logger.warning(
                "plugins.client_failed name=%s op=is_configured: %s",
                self._plugin_name,
                exc,
            )
            return False

    async def health_check(self) -> ServiceStatus:
        try:
            return await self._instance.health_check()  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - degraded, never fatal
            logger.warning(
                "plugins.client_failed name=%s op=health_check: %s",
                self._plugin_name,
                exc,
            )
            return ServiceStatus(status="error", message=f"plugin {self._plugin_name} unavailable")

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        payload = getattr(request, "payload", "") or ""
        if isinstance(payload, str) and payload:
            self._remember_token(request.task_id, payload)
            job_name = getattr(request, "job_name", "") or ""
            if job_name:
                self._remember_token(job_name, payload)
        try:
            handle = await self._instance.enqueue(request)  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - a plugin fails the download, not the loop
            logger.warning(
                "plugins.client_failed name=%s op=enqueue: %s", self._plugin_name, exc
            )
            raise _orchestration_error(
                f"plugin {self._plugin_name} enqueue failed"
            ) from exc
        self._attach_token(handle, request.task_id)
        return handle

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        self._attach_token(handle)
        try:
            return await self._instance.get_status(handle)  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - poll continues with other sources
            logger.warning(
                "plugins.client_failed name=%s op=get_status: %s", self._plugin_name, exc
            )
            raise _orchestration_error(
                f"plugin {self._plugin_name} status check failed"
            ) from exc

    async def abort(self, handle: TaskHandle) -> bool:
        self._attach_token(handle)
        try:
            return bool(await self._instance.abort(handle))  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - abort failure fails the task, not the loop
            logger.warning(
                "plugins.client_failed name=%s op=abort: %s", self._plugin_name, exc
            )
            raise _orchestration_error(
                f"plugin {self._plugin_name} abort failed"
            ) from exc

    async def inspect_materialization(
        self, handle: TaskHandle
    ) -> DownloadMaterialization:
        self._attach_token(handle)
        try:
            return await self._instance.inspect_materialization(handle)  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - cleanup retries without evidence
            logger.warning(
                "plugins.client_failed name=%s op=inspect_materialization",
                self._plugin_name,
            )
            raise _orchestration_error(
                f"plugin {self._plugin_name} materialization inspection failed"
            ) from exc

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        self._attach_token(handle)
        try:
            return bool(await self._instance.discard_client_artifacts(handle))  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - cleanup retries later
            logger.warning(
                "plugins.client_failed name=%s op=discard_client_artifacts: %s",
                self._plugin_name,
                exc,
            )
            return False

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        self._attach_token(handle)
        try:
            return await self._instance.list_completed_files(handle)  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - absence, the orchestrator falls back
            logger.warning(
                "plugins.client_failed name=%s op=list_completed_files: %s",
                self._plugin_name,
                exc,
            )
            return []

    async def get_file_path(
        self,
        handle: TaskHandle,
        remote_filename: str,
        size: int | None = None,
    ) -> Path | None:
        self._attach_token(handle)
        try:
            return await self._instance.get_file_path(handle, remote_filename, size)  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - absence, the orchestrator falls back
            logger.warning(
                "plugins.client_failed name=%s op=get_file_path: %s",
                self._plugin_name,
                exc,
            )
            return None

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        try:
            return await self._instance.diagnose_downloads_mount()  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - a diagnostic must never raise
            logger.warning(
                "plugins.client_failed name=%s op=diagnose_downloads_mount: %s",
                self._plugin_name,
                exc,
            )
            return MountDiagnosis(supported=False)

    def _remember_token(self, key: str, token: str) -> None:
        self._tokens[key] = token
        while len(self._tokens) > _TOKEN_CAP:
            self._tokens.pop(next(iter(self._tokens)))

    def _attach_token(self, handle: TaskHandle, task_id: str = "") -> None:
        """Reattach the enqueue-time opaque token so the plugin can match
        the handle to its own records. ``plugin_token`` lands on
        ``TaskHandle`` with the v1 protocols; before that the setattr is a
        no-op and the in-memory dict still correlates by job name."""
        token = self._tokens.get(task_id) if task_id else None
        if token is None:
            for key in (getattr(handle, "job_name", ""), getattr(handle, "nzo_id", "")):
                if key and key in self._tokens:
                    token = self._tokens[key]
                    break
        if token is None:
            return
        for key in {task_id, getattr(handle, "job_name", ""), getattr(handle, "nzo_id", "")}:
            if key:
                self._remember_token(key, token)
        try:
            handle.plugin_token = token  # type: ignore[attr-defined]
        except AttributeError:
            pass


class PluginIndexerAdapter:
    """``IndexerProtocol`` shim over one plugin instance.

    ``indexer_name`` is forced to the target source key (``"usenet"`` for
    usenet-targeting indexers pooling into the composite, else the owning
    plugin's ``plugin:<name>`` key); a raising or slow plugin drops its own
    result group (``[]``), never the search.
    """

    def __init__(self, *, plugin_name: str, target: str, instance: object) -> None:
        self._plugin_name = plugin_name
        self._target = target
        self._instance: object = instance

    @property
    def indexer_name(self) -> str:
        return self._target

    def is_configured(self) -> bool:
        try:
            return bool(self._instance.is_configured())  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - absence, not failure
            logger.warning(
                "plugins.indexer_failed name=%s op=is_configured: %s",
                self._plugin_name,
                exc,
            )
            return False

    async def health_check(self) -> ServiceStatus:
        try:
            return await self._instance.health_check()  # type: ignore[union-attr]
        except Exception as exc:  # noqa: BLE001 - degraded, never fatal
            logger.warning(
                "plugins.indexer_failed name=%s op=health_check: %s",
                self._plugin_name,
                exc,
            )
            return ServiceStatus(status="error", message=f"plugin {self._plugin_name} unavailable")

    async def search_album(
        self,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        try:
            async with asyncio.timeout(timeout):
                results = await self._instance.search_album(  # type: ignore[union-attr]
                    artist_name, album_title, year, track_count, timeout=timeout
                )
            return list(results) if results else []
        except Exception as exc:  # noqa: BLE001 - one broken indexer drops its group only
            logger.warning(
                "plugins.indexer_failed name=%s op=search_album: %s",
                self._plugin_name,
                exc,
            )
            return []

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        try:
            async with asyncio.timeout(timeout):
                results = await self._instance.search_track(  # type: ignore[union-attr]
                    artist_name, track_title, album_title, duration_seconds,
                    timeout=timeout,
                )
            return list(results) if results else []
        except Exception as exc:  # noqa: BLE001 - one broken indexer drops its group only
            logger.warning(
                "plugins.indexer_failed name=%s op=search_track: %s",
                self._plugin_name,
                exc,
            )
            return []
