"""``QbittorrentDownloadClient`` - the download side of the torrent source: a
``DownloadClientProtocol`` impl over ``QbittorrentClient``.

enqueue = add by magnet/.torrent URL with a correlation ``tag`` (= ``job_name``,
the PRE-enqueue key; ``torrents/add`` returns no hash) → recover the hash from
``torrents/info?tag=…`` → ``TaskHandle{job_name, torrent_hash}``. get_status maps
qBittorrent states; **a torrent at 100% reports ``completed`` even while it keeps
seeding** - seeding is a tracker obligation, not part of the download lifecycle.

**Private-tracker rule (cancel):** post-import cleanup and user cancellation call
``cancel``. A torrent that finished downloading is NEVER deleted (it must keep
seeding; the import COPIES files, see ``TorrentStrategy``); only an incomplete
torrent is removed WITH its partial data. list_completed_files remaps qBittorrent's
``content_path`` (its namespace) onto the DroppedNeedle downloads mount by
stripping the ``save_path`` prefix, then enumerates audio files (the folder-based
import source, D18).

No ``from __future__ import annotations`` (the conformance test compares real
signatures).
"""

import asyncio
import logging
from pathlib import Path, PurePosixPath

from models.common import ServiceStatus
from repositories.protocols.download_client import (
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    TaskHandle,
)

from .qbittorrent_client import QbittorrentApiError, QbittorrentClient
from .qbittorrent_models import QbtTorrentInfo

logger = logging.getLogger(__name__)

# Mirror of library_manager._AUDIO_SUFFIXES (kept local for layering, like SABnzbd's).
_AUDIO_SUFFIXES = {".flac", ".mp3", ".m4a", ".m4b", ".mp4", ".ogg", ".oga", ".opus", ".wav"}

_FAILED_STATES = {"error", "missingfiles"}
# Downloading-phase states that move payload bytes right now.
_ACTIVE_STATES = {"downloading", "forceddl"}
# Downloading-phase states that legitimately move 0 bytes (waiting/stalled/paused).
_WAITING_STATES = {"metadl", "stalleddl", "queueddl", "pauseddl", "stoppeddl", "allocating"}
_CHECKING_STATES = {"checkingdl", "checkingup", "checkingresumedata", "moving"}
# Post-completion (seeding/idle) states - the download itself is done.
_SEEDING_STATES = {"uploading", "stalledup", "queuedup", "pausedup", "stoppedup", "forcedup"}

# How many correlation polls to give a just-added torrent before the add is declared
# failed (magnet resolution can take a few seconds before the row appears).
_ADD_CORRELATE_ATTEMPTS = 10
_ADD_CORRELATE_DELAY = 1.0


class QbittorrentDownloadClient:
    _DIAGNOSIS_SAMPLE = 3

    def __init__(
        self,
        client: QbittorrentClient,
        url: str,
        api_key: str,
        downloads_mount: Path,
        *,
        category: str = "droppedneedle",
    ) -> None:
        self._client = client
        self._url = url
        self._api_key = api_key
        self._mount = Path(downloads_mount)
        self._category = category

    @property
    def client_name(self) -> str:
        return "qbittorrent"

    @property
    def supported_sources(self) -> frozenset[str]:
        return frozenset({"torrent"})

    def is_configured(self) -> bool:
        return bool(self._url and self._api_key)

    async def health_check(self) -> ServiceStatus:
        try:
            version = await self._client.version()
        except Exception as exc:  # noqa: BLE001 - health check never raises
            return ServiceStatus(status="error", message=str(exc))
        if not _supports_api_key(version):
            return ServiceStatus(
                status="error",
                version=version or None,
                message="qBittorrent 5.2 or newer is required for API-key authentication",
            )
        return ServiceStatus(
            status="ok",
            version=version or None,
            message=f"qBittorrent {version}" if version else "qBittorrent",
        )

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        urls = [url for url in (request.magnet_uri, request.torrent_url) if url]
        if not urls:
            raise QbittorrentApiError(
                "enqueue requires a magnet_uri or torrent_url for the torrent source"
            )
        correlation_id = request.job_name or f"droppedneedle-{request.task_id}"
        existing = await self._recover(request, correlation_id)
        if existing is not None:
            return self._handle(existing, correlation_id)

        last_error: QbittorrentApiError | None = None
        for url in dict.fromkeys(urls):
            try:
                await self._client.add_torrent(
                    urls=url, category=self._category, tag=correlation_id
                )
            except QbittorrentApiError as exc:
                last_error = exc
                existing = await self._recover(request, correlation_id)
                if existing is not None:
                    return self._handle(existing, correlation_id)
                continue
            for _ in range(_ADD_CORRELATE_ATTEMPTS):
                existing = await self._recover(request, correlation_id)
                if existing is not None:
                    return self._handle(existing, correlation_id)
                await asyncio.sleep(_ADD_CORRELATE_DELAY)
        if last_error is not None:
            raise last_error
        raise QbittorrentApiError(
            "qBittorrent accepted the add but no torrent appeared"
        )

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        info = await self._find(handle)
        if info is None:
            # Not visible yet (just-added) or removed out-of-band.
            return DownloadTaskStatus(task_id="", status="queued", matched_transfers=0)
        return _map_status(info)

    async def cancel(self, handle: TaskHandle) -> bool:
        """Private-tracker rule: a COMPLETED torrent is never deleted - it keeps
        seeding under its category (the import copied the files). Only an incomplete
        torrent is removed, WITH its partial data."""
        info = await self._find(handle)
        if info is None:
            return False
        if info.progress >= 1.0:
            logger.info(
                "qbittorrent: leaving completed torrent %s seeding (no delete)", info.hash
            )
            return True
        return await self._client.delete_torrents(info.hash, delete_files=True)

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        info = await self._find(handle)
        if info is None or info.progress < 1.0 or not info.content_path:
            return []
        local = self._local_path(info)
        return await asyncio.to_thread(self._enumerate_audio, local)

    async def get_file_path(
        self, handle: TaskHandle, remote_filename: str, size: int | None = None
    ) -> Path | None:
        files = await self.list_completed_files(handle)
        basename = remote_filename.replace("\\", "/").rsplit("/", 1)[-1]
        for path in files:
            if path.name == basename:
                return path
        if size is not None:
            for path in files:
                try:
                    if path.stat().st_size == size:
                        return path
                except OSError:
                    continue
        return None

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        try:
            rows = await self._client.torrents_info(category=self._category)
        except Exception:  # noqa: BLE001 - a diagnostic must never raise
            return MountDiagnosis(supported=True)
        completed = [r for r in rows if r.progress >= 1.0 and r.content_path]
        if not completed:
            return MountDiagnosis(
                supported=True, completed_downloads=0, mount_has_files=True
            )
        sample = completed[: self._DIAGNOSIS_SAMPLE]
        resolvable = 0
        for info in sample:
            local = self._local_path(info)
            if await asyncio.to_thread(_path_has_file, local):
                resolvable += 1
        has_files = await asyncio.to_thread(self._mount_has_any_file)
        client_dir = completed[0].save_path or None
        return MountDiagnosis(
            supported=True,
            completed_downloads=len(completed),
            mount_has_files=has_files,
            resolvable_downloads=resolvable,
            sampled_downloads=len(sample),
            client_downloads_dir=client_dir,
        )

    async def downloads_mount_healthy(self) -> bool:
        """Whether DroppedNeedle's downloads MOUNT itself is usable (mirrors
        ``SabnzbdDownloadClient.downloads_mount_healthy`` - see its docstring for
        why only the mount root is checked, never the per-job folder)."""

        def _ok() -> bool:
            try:
                if not self._mount.is_dir():
                    return False
                next(self._mount.iterdir(), None)
                return True
            except OSError:
                return False

        return await asyncio.to_thread(_ok)

    # --- internals --------------------------------------------------------------

    async def _recover(
        self, request: EnqueueRequest, correlation_id: str
    ) -> QbtTorrentInfo | None:
        rows = await self._client.torrents_info(tag=correlation_id)
        if rows:
            return rows[0]
        if request.content_id:
            rows = await self._client.torrents_info(hashes=request.content_id)
            if rows:
                return rows[0]
        return None


    @staticmethod
    def _handle(info: QbtTorrentInfo, correlation_id: str) -> TaskHandle:
        return TaskHandle(
            source="torrent",
            job_name=correlation_id,
            external_id=info.hash,
            correlation_id=correlation_id,
        )

    async def _find(self, handle: TaskHandle) -> QbtTorrentInfo | None:
        if handle.external_id:
            rows = await self._client.torrents_info(hashes=handle.external_id)
            if rows:
                return rows[0]
        if handle.correlation_id:
            rows = await self._client.torrents_info(tag=handle.correlation_id)
            if rows:
                return rows[0]
        return None

    def _local_path(self, info: QbtTorrentInfo) -> Path:
        """Remap qBittorrent's ``content_path`` (its namespace) onto the DroppedNeedle
        mount by stripping the ``save_path`` prefix; fall back to the basename."""
        remote = PurePosixPath(info.content_path)
        if info.save_path:
            try:
                rel = remote.relative_to(PurePosixPath(info.save_path))
                return self._mount / Path(*rel.parts)
            except ValueError:
                pass
        return self._mount / remote.name

    def _enumerate_audio(self, root_path: Path) -> list[Path]:
        """Audio files under the torrent's content path (bounded DFS), confined to
        the mount. A single-file torrent returns just that file."""
        mount = self._mount.resolve()
        try:
            root = root_path.resolve()
        except OSError:
            return []
        if not root.is_relative_to(mount):
            return []
        if root.is_file():
            return [root] if root.suffix.lower() in _AUDIO_SUFFIXES else []
        if not root.is_dir():
            return []
        out: list[Path] = []
        stack = [root]
        seen = 0
        while stack:
            try:
                entries = list(stack.pop().iterdir())
            except OSError:
                continue
            for entry in entries:
                seen += 1
                if seen > 10000:
                    return out
                if entry.is_dir():
                    stack.append(entry)
                elif entry.is_file() and entry.suffix.lower() in _AUDIO_SUFFIXES:
                    out.append(entry)
        return out

    def _mount_has_any_file(self) -> bool:
        try:
            stack = [self._mount]
            seen = 0
            while stack:
                for entry in stack.pop().iterdir():
                    seen += 1
                    if seen > 5000:
                        return True
                    if entry.is_file():
                        return True
                    if entry.is_dir():
                        stack.append(entry)
        except OSError:
            return False
        return False


def _path_has_file(path: Path) -> bool:
    try:
        if path.is_file():
            return True
        return path.is_dir() and any(p.is_file() for p in path.iterdir())
    except OSError:
        return False


def _supports_api_key(version: str) -> bool:
    """qBittorrent added Bearer API-key authentication in 5.2.0."""
    import re

    match = re.search(r"(\d+)\.(\d+)", version or "")
    return bool(match and (int(match.group(1)), int(match.group(2))) >= (5, 2))


def _map_status(info: QbtTorrentInfo) -> DownloadTaskStatus:
    state = info.state.lower()
    bytes_total = info.size
    bytes_downloaded = min(info.downloaded, bytes_total) if bytes_total else info.downloaded
    percent = round(info.progress * 100.0, 1)
    if state in _FAILED_STATES:
        return DownloadTaskStatus(
            task_id="", status="failed",
            error="qBittorrent reported an error for this torrent",
            bytes_total=bytes_total, matched_transfers=1,
        )
    if info.progress >= 1.0 or state in _SEEDING_STATES:
        # Download done - seeding continues in qBittorrent but the lifecycle here is over.
        return DownloadTaskStatus(
            task_id="", status="completed",
            files_total=1, files_completed=1,
            bytes_total=bytes_total, bytes_downloaded=bytes_total,
            progress_percent=100.0, matched_transfers=1,
        )
    if state in _CHECKING_STATES:
        return DownloadTaskStatus(
            task_id="", status="processing",
            files_total=1,
            bytes_total=bytes_total, bytes_downloaded=bytes_downloaded,
            progress_percent=percent, matched_transfers=1,
        )
    if state in _ACTIVE_STATES:
        return DownloadTaskStatus(
            task_id="", status="downloading",
            files_total=1,
            bytes_total=bytes_total, bytes_downloaded=bytes_downloaded,
            progress_percent=percent,
            has_active_transfer=True, matched_transfers=1,
        )
    # metaDL / stalled / queued / paused / allocating / unknown: waiting, 0-byte states.
    return DownloadTaskStatus(
        task_id="", status="queued",
        files_total=1,
        bytes_total=bytes_total, bytes_downloaded=bytes_downloaded,
        progress_percent=percent, matched_transfers=1,
    )
