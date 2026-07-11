"""Soulseek acquisition via a user-supplied slskd instance, as a plugin.

DELEGATES to the pre-existing client code (``repositories/slskd``) through the
same DI singletons the orchestrator used before the plugin layer existed
(``get_slskd_repository`` / ``get_slskd_indexer``), resolved lazily per call so
a settings save (which cache-clears those singletons) is picked up without a
restart. Behaviour is therefore byte-identical to the pre-plugin wiring.

Settings and the enable flag proxy the legacy ``download_client`` preferences
section, so the existing ``/download-client/*`` admin routes and the plugin
settings API read/write the same values.
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
    SettingsField,
    TaskHandle,
    TestResult,
)


class SoulseekSlskdPlugin(AcquisitionPlugin):
    id = "soulseek"
    name = "Soulseek (slskd)"
    version = "1.0.0"

    # ------------------------------------------------------------------ helpers

    @staticmethod
    def _repo():
        from core.dependencies import get_slskd_repository

        return get_slskd_repository()

    @staticmethod
    def _indexer():
        from core.dependencies import get_slskd_indexer

        return get_slskd_indexer()

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
                label="slskd URL",
                help="Base URL of your slskd instance, e.g. http://slskd:5030",
                default="",
                required=True,
            ),
            SettingsField(
                key="api_key",
                type="secret",
                label="API key",
                help="slskd API key (Options > Web > Authentication).",
                default="",
                required=True,
            ),
            SettingsField(
                key="downloads_subpath",
                type="str",
                label="Downloads subfolder",
                help=(
                    "Optional subfolder inside the downloads mount where slskd saves "
                    "completed files, for when the mount points at a parent folder."
                ),
                default="",
            ),
        ]

    def configure(self, settings: dict[str, Any]) -> None:
        # Reads the live preferences section on every call; nothing to hold.
        return None

    async def test_connection(self, settings: dict[str, Any] | None = None) -> TestResult:
        from core.dependencies import build_slskd_repository
        from infrastructure.validators import validate_service_url
        from core.exceptions import ValidationError

        current = self._prefs().get_download_client_settings_raw()
        values = settings or {"url": current.url, "api_key": current.api_key}
        url = str(values.get("url") or "")
        api_key = str(values.get("api_key") or "")
        try:
            validate_service_url(url, label="slskd URL")
        except ValidationError as exc:
            return TestResult(ok=False, message=str(exc))
        try:
            status = await build_slskd_repository(url, api_key).health_check()
        except Exception as exc:  # noqa: BLE001 - a failed test is a result, not a 500
            return TestResult(ok=False, message=f"slskd unreachable: {exc}")
        return TestResult(
            ok=status.status == "ok",
            message=status.message or (f"slskd {status.version}" if status.version else ""),
            version=status.version,
        )

    # -------------------------------------------------------------- capabilities

    def is_configured(self) -> bool:
        return self._repo().is_configured()

    async def health_check(self):
        return await self._repo().health_check()

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
        return await self._repo().enqueue(request)

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        return await self._repo().get_status(handle)

    async def cancel(self, handle: TaskHandle) -> bool:
        return await self._repo().cancel(handle)

    async def completed_path(self, handle: TaskHandle) -> list[Path]:
        return await self._repo().list_completed_files(handle)

    async def get_file_path(
        self, handle: TaskHandle, remote_filename: str, size: int | None = None
    ) -> Path | None:
        return await self._repo().get_file_path(handle, remote_filename, size)

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        return await self._repo().diagnose_downloads_mount()

    # -------------------------------------------------- orchestrator integration

    def get_indexer(self):
        # The exact pre-plugin object (SlskdIndexer singleton) - lossless wrapping.
        return self._indexer()

    def get_download_client(self):
        # The exact pre-plugin object (SlskdRepository singleton) - lossless wrapping.
        return self._repo()

    # ------------------------------------------------------- built-in overrides

    def enabled_override(self) -> bool | None:
        return bool(self._prefs().get_download_client_settings().enabled)

    def apply_enabled(self, enabled: bool) -> bool:
        from core.dependencies.invalidation import clear_soulseek_chain

        prefs = self._prefs()
        current = prefs.get_download_client_settings_raw()
        current.enabled = enabled
        prefs.save_download_client_settings(current)
        clear_soulseek_chain()
        return True

    def settings_values(self) -> dict[str, Any] | None:
        raw = self._prefs().get_download_client_settings_raw()
        return {
            "url": raw.url,
            "api_key": raw.api_key,
            "downloads_subpath": raw.downloads_subpath,
        }

    def apply_settings(self, values: dict[str, Any]) -> bool:
        from core.dependencies.invalidation import clear_soulseek_chain

        prefs = self._prefs()
        current = prefs.get_download_client_settings_raw()
        current.url = str(values.get("url", current.url) or "")
        current.api_key = str(values.get("api_key", current.api_key) or "")
        current.downloads_subpath = str(
            values.get("downloads_subpath", current.downloads_subpath) or ""
        )
        prefs.save_download_client_settings(current)
        clear_soulseek_chain()
        return True
