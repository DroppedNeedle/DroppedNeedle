"""PluginSourceService - the bridge from audio-source plugins to the library.

Search proxies straight to the plugin. Fetch runs in the background (source
files can be gigabytes) into a private staging dir, then hands the result to
the drop importer - the same identify/organise/notify pipeline every import
uses (phase 01c). The plugin never touches the library; it only ever fills a
directory the importer then owns.
"""

import asyncio
import logging
import uuid
from pathlib import Path
from typing import TYPE_CHECKING

from infrastructure.plugins.protocols import SourceItem

if TYPE_CHECKING:
    from infrastructure.plugins.host import PluginHost
    from infrastructure.sse_publisher import SSEPublisher
    from services.native.drop_import_service import DropImportService

logger = logging.getLogger(__name__)


class PluginSourceService:
    def __init__(
        self,
        *,
        plugin_host: "PluginHost",
        drop_import_service: "DropImportService",
        sse_publisher: "SSEPublisher",
    ) -> None:
        self._host = plugin_host
        self._drop_import = drop_import_service
        self._sse = sse_publisher
        self._tasks: set[asyncio.Task] = set()

    def list_sources(self) -> list:
        return self._host.sources()

    async def search(self, plugin_name: str, query: str) -> list[SourceItem]:
        return await self._host.source_search(plugin_name, query)

    def start_fetch(
        self, plugin_name: str, item_id: str, *, user_id: str, user_name: str
    ) -> None:
        """Kick the fetch-and-import in the background; progress then lives in
        the ordinary drop-import job list once the files have landed."""
        self._host.require_source(plugin_name)  # fail fast on a bad plugin name
        task = asyncio.create_task(
            self._fetch_and_import(plugin_name, item_id, user_id, user_name)
        )
        self._tasks.add(task)
        task.add_done_callback(self._on_done)

    def _on_done(self, task: asyncio.Task) -> None:
        self._tasks.discard(task)
        if not task.cancelled() and task.exception() is not None:
            logger.error("plugin source fetch crashed", exc_info=task.exception())

    async def _fetch_and_import(
        self, plugin_name: str, item_id: str, user_id: str, user_name: str
    ) -> None:
        import shutil

        dest = self._drop_import.incoming_dir() / f"source-{uuid.uuid4().hex}"
        try:
            try:
                files = await self._host.source_fetch(plugin_name, item_id, dest)
            except Exception as exc:  # noqa: BLE001 - surfaced to the user, never raised
                logger.warning(
                    "plugins.source_fetch_failed plugin=%s item=%s: %s",
                    plugin_name, item_id, exc,
                )
                await self._notify_failure(
                    user_id, plugin_name, "The fetch failed - check the logs"
                )
                return
            audio = [f for f in files if isinstance(f, Path) and f.is_file()]
            if not audio:
                await self._notify_failure(
                    user_id, plugin_name, "The source returned no files"
                )
                return
            await self._drop_import.create_job(
                user_id=user_id,
                user_name=user_name,
                uploads=[(f.name, f) for f in audio],
            )
        finally:
            # create_job moved the audio out; whatever remains (the dir itself,
            # partial downloads after a failure) must not accumulate forever
            await asyncio.to_thread(shutil.rmtree, dest, True)

    async def _notify_failure(self, user_id: str, plugin_name: str, message: str) -> None:
        try:
            await self._sse.publish(
                f"user:{user_id}",
                "plugin_fetch_failed",
                {
                    "event_id": uuid.uuid4().hex,
                    "plugin": plugin_name,
                    "message": message,
                },
            )
        except Exception as exc:  # noqa: BLE001 - notification is best-effort
            logger.debug("plugin_fetch_failed publish failed: %s", exc)
