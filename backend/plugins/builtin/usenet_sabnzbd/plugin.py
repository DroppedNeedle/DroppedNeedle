"""Usenet acquisition (SABnzbd download client + Newznab indexer search), as a plugin.

DELEGATES to the pre-existing client code (``repositories/sabnzbd`` and
``repositories/newznab``) through the same DI singletons the orchestrator used
before the plugin layer existed (``get_sabnzbd_download_client`` /
``get_newznab_indexer``), resolved lazily per call so a settings save (which
cache-clears those singletons) is picked up without a restart. Behaviour is
therefore byte-identical to the pre-plugin wiring.

The plugin owns the SABnzbd connection settings (proxying the legacy
``download_clients.sabnzbd`` preferences section). The Newznab indexer LIST
stays a separate admin surface (``/api/v1/indexers``) - indexers are per-user
sources, not plugin config - but this plugin's ``search`` capability fans out
across them, and its readiness (``is_configured``) mirrors the pre-plugin rule:
SABnzbd reachable AND at least one enabled indexer.
"""

from pathlib import Path
from typing import Any

from plugins.base import (
    AcquisitionPlugin,
    Candidate,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    SearchRequest,
    SelectOption,
    SettingsField,
    TaskHandle,
    TestResult,
)


class UsenetSabnzbdPlugin(AcquisitionPlugin):
    id = "usenet"
    name = "Usenet (SABnzbd)"
    version = "1.0.0"

    # ------------------------------------------------------------------ helpers

    @staticmethod
    def _client():
        from core.dependencies import get_sabnzbd_download_client

        return get_sabnzbd_download_client()

    @staticmethod
    def _indexer():
        from core.dependencies import get_newznab_indexer

        return get_newznab_indexer()

    @staticmethod
    def _prefs():
        from core.dependencies import get_preferences_service

        return get_preferences_service()

    # ---------------------------------------------------------------- lifecycle

    def settings_schema(self) -> list[SettingsField]:
        return [
            SettingsField(
                key="url",
                type="str",
                label="SABnzbd URL",
                help="Base URL of your SABnzbd instance, e.g. http://sabnzbd:8080",
                default="",
                required=True,
            ),
            SettingsField(
                key="api_key",
                type="secret",
                label="API key",
                help="The FULL SABnzbd API key (the add-only NZB key can't manage the queue).",
                default="",
                required=True,
            ),
            SettingsField(
                key="category",
                type="str",
                label="Category",
                help="SABnzbd category new downloads are filed under ('*' = default).",
                default="*",
            ),
            SettingsField(
                key="priority",
                type="select",
                label="Priority",
                help="Queue priority SABnzbd assigns to DroppedNeedle downloads.",
                default=0,
                options=[
                    SelectOption(value=-1, label="Low"),
                    SelectOption(value=0, label="Normal"),
                    SelectOption(value=1, label="High"),
                    SelectOption(value=2, label="Force"),
                ],
            ),
            SettingsField(
                key="post_processing",
                type="select",
                label="Post-processing",
                help="How far SABnzbd processes a finished job before hand-off.",
                default=3,
                options=[
                    SelectOption(value=0, label="Skip"),
                    SelectOption(value=1, label="Repair"),
                    SelectOption(value=2, label="Repair + Unpack"),
                    SelectOption(value=3, label="Repair + Unpack + Delete"),
                ],
            ),
            SettingsField(
                key="downloads_mount",
                type="str",
                label="Downloads mount",
                help="Where DroppedNeedle sees SABnzbd's completed-downloads folder.",
                default="/sabnzbd-downloads",
            ),
        ]

    def configure(self, settings: dict[str, Any]) -> None:
        # Reads the live preferences section on every call; nothing to hold.
        return None

    async def test_connection(self, settings: dict[str, Any] | None = None) -> TestResult:
        from core.dependencies import build_sabnzbd_download_client
        from core.exceptions import ExternalServiceError

        current = self._prefs().get_sabnzbd_connection_raw()
        values = settings or {"url": current.url, "api_key": current.api_key}
        url = str(values.get("url") or "")
        api_key = str(values.get("api_key") or "")
        if not url:
            return TestResult(ok=False, message="SABnzbd URL is required")
        client = build_sabnzbd_download_client(url, api_key)
        try:
            status = await client.health_check()
        except ExternalServiceError as exc:
            return TestResult(ok=False, message=str(exc))
        except Exception as exc:  # noqa: BLE001 - a failed test is a result, not a 500
            return TestResult(ok=False, message=f"SABnzbd unreachable: {exc}")
        if status.status != "ok":
            return TestResult(
                ok=False, message=status.message or "SABnzbd unreachable", version=status.version
            )
        return TestResult(ok=True, message=f"SABnzbd {status.version}", version=status.version)

    # -------------------------------------------------------------- capabilities

    def is_configured(self) -> bool:
        # Mirrors the pre-plugin readiness rule: SABnzbd configured AND at least
        # one enabled indexer to search (is_usenet_ready minus the enable flag).
        return self._client().is_configured() and any(
            i.enabled for i in self._prefs().get_indexers()
        )

    async def health_check(self):
        return await self._client().health_check()

    async def search(self, request: SearchRequest) -> list[Candidate]:
        indexer = self._indexer()
        if request.kind == "track":
            return await indexer.search_track(
                request.artist_name,
                request.track_title or "",
                request.album_title,
                request.duration_seconds,
                timeout=request.timeout,
            )
        return await indexer.search_album(
            request.artist_name,
            request.album_title or "",
            request.year,
            request.track_count,
            timeout=request.timeout,
        )

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        return await self._client().enqueue(request)

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        return await self._client().get_status(handle)

    async def cancel(self, handle: TaskHandle) -> bool:
        return await self._client().cancel(handle)

    async def completed_path(self, handle: TaskHandle) -> list[Path]:
        return await self._client().list_completed_files(handle)

    async def get_file_path(
        self, handle: TaskHandle, remote_filename: str, size: int | None = None
    ) -> Path | None:
        return await self._client().get_file_path(handle, remote_filename, size)

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        return await self._client().diagnose_downloads_mount()

    # -------------------------------------------------- orchestrator integration

    def get_indexer(self):
        # The exact pre-plugin object (NewznabIndexer singleton) - lossless wrapping.
        return self._indexer()

    def get_download_client(self):
        # The exact pre-plugin object (SabnzbdDownloadClient singleton) - lossless wrapping.
        return self._client()

    # ------------------------------------------------------- built-in overrides

    def enabled_override(self) -> bool | None:
        return bool(self._prefs().get_sabnzbd_connection().enabled)

    def apply_enabled(self, enabled: bool) -> bool:
        from core.dependencies.invalidation import clear_usenet_chain

        prefs = self._prefs()
        current = prefs.get_sabnzbd_connection_raw()
        current.enabled = enabled
        prefs.save_sabnzbd_connection(current)
        clear_usenet_chain()
        return True

    def settings_values(self) -> dict[str, Any] | None:
        raw = self._prefs().get_sabnzbd_connection_raw()
        return {
            "url": raw.url,
            "api_key": raw.api_key,
            "category": raw.category,
            "priority": raw.priority,
            "post_processing": raw.post_processing,
            "downloads_mount": raw.downloads_mount,
        }

    def apply_settings(self, values: dict[str, Any]) -> bool:
        from core.dependencies.invalidation import clear_usenet_chain

        prefs = self._prefs()
        current = prefs.get_sabnzbd_connection_raw()
        current.url = str(values.get("url", current.url) or "")
        current.api_key = str(values.get("api_key", current.api_key) or "")
        current.category = str(values.get("category", current.category) or "*")
        current.priority = int(values.get("priority", current.priority))
        current.post_processing = int(values.get("post_processing", current.post_processing))
        current.downloads_mount = str(
            values.get("downloads_mount", current.downloads_mount) or ""
        )
        prefs.save_sabnzbd_connection(current)
        clear_usenet_chain()
        return True
