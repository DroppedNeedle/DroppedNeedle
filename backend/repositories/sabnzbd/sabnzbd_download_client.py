"""``SabnzbdDownloadClient`` - the download side of Usenet (D2): a
``DownloadClientProtocol`` impl over ``SabnzbdClient``.

enqueue = fetch the release NZB → validate → ``addfile`` → ``TaskHandle{job_name,
nzo_id}`` (job_name = ``droppedneedle-{task_id}`` is the PRE-enqueue key). get_status
walks queue→history; **only the true ``Downloading`` state sets
``has_active_transfer``** (so Grabbing/Queued/Paused/post-processing don't trip the
orchestrator's stall/queued watchdogs - ``05-…`` §Poll). list_completed_files remaps
``storage`` (SABnzbd namespace) onto the DroppedNeedle downloads mount and enumerates
the audio files (the folder-based import source, D18). Completed materialization is
inspected separately from abort/history removal so local cleanup remains durable.

No ``from __future__ import annotations`` (the conformance test compares real
signatures).
"""

import asyncio
import logging
from pathlib import Path, PurePosixPath

from models.common import ServiceStatus
from repositories.protocols.download_client import (
    DownloadMaterialization,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    TaskHandle,
)

from .sabnzbd_client import SabnzbdApiError, SabnzbdClient
from .sabnzbd_models import SabnzbdHistorySlot, SabnzbdQueueSlot

logger = logging.getLogger(__name__)

# Mirror of library_manager._AUDIO_SUFFIXES (the importer's accepted set). Kept local
# so this repository doesn't import from services/native (layering).
_AUDIO_SUFFIXES = {".flac", ".mp3", ".m4a", ".m4b", ".mp4", ".ogg", ".oga", ".opus", ".wav"}

# SABnzbd queue states that move 0 bytes (NOT active transfers). Everything else in the
# queue that isn't true "Downloading" is post-processing -> "processing".
_QUEUE_NOT_ACTIVE = {"queued", "grabbing", "propagating", "paused"}
_HISTORY_LIMIT = 50


class SabnzbdDownloadClient:
    _DIAGNOSIS_SAMPLE = 3

    def __init__(
        self,
        client: SabnzbdClient,
        url: str,
        api_key: str,
        downloads_mount: Path,
    ) -> None:
        self._client = client
        self._url = url
        self._api_key = api_key
        self._mount = Path(downloads_mount)
        self._complete_dir_cache: str | None = None

    @property
    def client_name(self) -> str:
        return "sabnzbd"

    def is_configured(self) -> bool:
        return bool(self._url and self._api_key)

    async def health_check(self) -> ServiceStatus:
        try:
            version = await self._client.version()
        except Exception as exc:  # noqa: BLE001 - health check never raises
            return ServiceStatus(status="error", message=str(exc))
        return ServiceStatus(
            status="ok", version=version, message=f"SABnzbd {version}" if version else "SABnzbd"
        )

    async def get_categories(self) -> list[str]:
        """SABnzbd's category names (for the settings picker)."""
        return await self._client.get_cats()

    async def get_complete_dir(self) -> str:
        """SABnzbd's completed-downloads dir (the mount-hint shown in settings)."""
        return await self._complete_dir()

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        if not request.nzb_url:
            raise SabnzbdApiError("enqueue requires an nzb_url for the usenet source")
        job_name = request.job_name or f"droppedneedle-{request.task_id}"
        nzb_bytes = await self._client.fetch_nzb(request.nzb_url)
        response = await self._client.add_file(
            job_name,
            nzb_bytes,
            category=request.category,
            priority=request.priority,
            post_processing=request.post_processing,
        )
        if not response.nzo_ids:
            raise SabnzbdApiError("SABnzbd rejected the NZB (no nzo_id returned)")
        return TaskHandle(source="usenet", job_name=job_name, nzo_id=response.nzo_ids[0])

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        queue = await self._client.queue()
        slot = self._unique_queue_slot(queue.slots, handle)
        if slot is not None:
            return _queue_status(slot)
        slot = await self._find_history_slot(handle)
        if slot is not None:
            return _history_status(slot)
        # Not in queue or history yet (just-added / between states) or gone.
        return DownloadTaskStatus(task_id="", status="queued", matched_transfers=0)

    async def abort(self, handle: TaskHandle) -> bool:
        nzo_id = await self._resolve_nzo_id(handle)
        if not nzo_id:
            return True
        queue = await self._client.queue()
        in_queue = any(s.nzo_id == nzo_id for s in queue.slots)
        if in_queue:
            return await self._client.delete_queue(nzo_id, del_files=True)
        slot = await self._find_history_slot(handle)
        if slot is None:
            return True
        if slot.status.lower() == "failed":
            return await self._client.delete_history(
                nzo_id, del_files=True, archive=False
            )
        # Completed output is not client-owned temporary data. The cleanup service must
        # validate and remove its exact workspace before the history record is discarded.
        return False

    async def inspect_materialization(
        self, handle: TaskHandle
    ) -> DownloadMaterialization:
        queue = await self._client.queue()
        queue_slot = self._unique_queue_slot(queue.slots, handle)
        if queue_slot is not None:
            return DownloadMaterialization(
                state="active",
                nzo_id=queue_slot.nzo_id,
                mount_root=str(self._mount),
                mount_healthy=await self.downloads_mount_healthy(),
            )
        slot = await self._find_history_slot(handle)
        mount_healthy = await self.downloads_mount_healthy()
        if slot is None:
            return DownloadMaterialization(
                state="missing",
                mount_root=str(self._mount),
                mount_healthy=mount_healthy,
            )
        local = await self._exact_local_storage(slot.storage) if slot.storage else None
        return DownloadMaterialization(
            state=("failed" if slot.status.lower() == "failed" else "completed"),
            nzo_id=slot.nzo_id,
            remote_storage=slot.storage,
            mount_root=str(self._mount),
            workspace_path=str(local) if local is not None else "",
            mount_healthy=mount_healthy,
        )

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        slot = await self._find_history_slot(handle)
        if slot is None:
            queue = await self._client.queue()
            if any(
                self._matches(item.nzo_id, item.filename, handle)
                for item in queue.slots
            ):
                return False
            return True
        # SABnzbd 5.0.4 and current releases apply history del_files only to a Failed
        # job's incomplete path. Completed output was removed locally and MUST use 0.
        return await self._client.delete_history(
            slot.nzo_id,
            del_files=slot.status.lower() == "failed",
            archive=False,
        )

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        slot = await self._find_history_slot(handle)
        if slot is None or not slot.storage:
            return []
        local = await self._local_storage(slot.storage)
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
        client_dir = await self._complete_dir()
        try:
            history = await self._client.history(limit=20)
        except Exception:  # noqa: BLE001 - a diagnostic must never raise
            return MountDiagnosis(supported=True, client_downloads_dir=client_dir or None)
        completed = [s for s in history.slots if s.status.lower() == "completed" and s.storage]
        if not completed:
            return MountDiagnosis(
                supported=True, completed_downloads=0, mount_has_files=True,
                client_downloads_dir=client_dir or None,
            )
        sample = completed[: self._DIAGNOSIS_SAMPLE]
        resolvable = 0
        for slot in sample:
            local = await self._local_storage(slot.storage)
            if await asyncio.to_thread(_dir_has_file, local):
                resolvable += 1
        has_files = await asyncio.to_thread(self._mount_has_any_file)
        return MountDiagnosis(
            supported=True,
            completed_downloads=len(completed),
            mount_has_files=has_files,
            resolvable_downloads=resolvable,
            sampled_downloads=len(sample),
            client_downloads_dir=client_dir or None,
        )

    # --- internals --------------------------------------------------------------

    @staticmethod
    def _matches(slot_id: str, slot_name: str, handle: TaskHandle) -> bool:
        if handle.nzo_id:
            return slot_id == handle.nzo_id
        return bool(handle.job_name) and slot_name == handle.job_name

    @classmethod
    def _unique_queue_slot(cls, slots, handle: TaskHandle):  # noqa: ANN001, ANN206
        matches = [
            slot
            for slot in slots
            if slot.status.lower() != "deleted"
            and cls._matches(slot.nzo_id, slot.filename, handle)
        ]
        if len(matches) > 1:
            raise SabnzbdApiError("SABnzbd returned an ambiguous job identity")
        return matches[0] if matches else None

    async def _resolve_nzo_id(self, handle: TaskHandle) -> str:
        if handle.nzo_id:
            return handle.nzo_id
        slot = await self._find_history_slot(handle)
        if slot is not None:
            return slot.nzo_id
        queue = await self._client.queue()
        slot = self._unique_queue_slot(queue.slots, handle)
        return slot.nzo_id if slot is not None else ""

    async def _find_history_slot(self, handle: TaskHandle) -> SabnzbdHistorySlot | None:
        # Filter to THIS job. nzo_id is SABnzbd's unique key, so when we have it query by it
        # ALONE - also passing search=job_name risks an AND that drops the row when SAB
        # renamed the job (category sorting, or a .1 dedup that truncated job_name out of the
        # name), making a completed job read as never-completed and falsely blocklist a good
        # release. job_name search is only the pre-enqueue crash-recovery fallback.
        history = await self._client.history(
            limit=_HISTORY_LIMIT,
            nzo_ids=handle.nzo_id or None,
            search=None if handle.nzo_id else (handle.job_name or None),
        )
        matches = [
            slot
            for slot in history.slots
            if self._matches(slot.nzo_id, slot.name, handle)
        ]
        if len(matches) > 1:
            raise SabnzbdApiError("SABnzbd returned an ambiguous job identity")
        return matches[0] if matches else None

    async def downloads_mount_healthy(self) -> bool:
        """Whether DroppedNeedle's downloads MOUNT itself is usable. False ONLY when the
        configured mount root is missing or unreadable (a real environment fault - failing
        over or blocklisting can't fix it). A healthy mount whose per-job folder is merely
        empty/absent is a RELEASE problem (garbage / incomplete NZB), NOT a mount fault, so
        this returns True and lets the orchestrator blocklist the bad release. (Checking the
        per-job dir was wrong: a malformed NZB that SABnzbd marks Completed with an empty
        folder looked identical to a broken mount.)"""

        def _ok() -> bool:
            try:
                if not self._mount.is_dir():
                    return False
                next(self._mount.iterdir(), None)  # force a readdir; raises if unreadable
                return True
            except OSError:
                return False

        return await asyncio.to_thread(_ok)

    async def _complete_dir(self) -> str:
        # Cache only a NON-EMPTY value: a transient get_config failure (or an empty result)
        # must not poison the cache and force the basename-only remap forever, which would
        # mis-resolve a per-category job folder (complete/<cat>/<job>).
        if self._complete_dir_cache:
            return self._complete_dir_cache
        try:
            config = await self._client.get_config()
            value = config.misc.complete_dir or ""
        except Exception:  # noqa: BLE001 - transient; retry next call
            return ""
        if value:
            self._complete_dir_cache = value
        return value

    async def _local_storage(self, storage: str) -> Path:
        """Remap SABnzbd's ``storage`` (its namespace) onto the DroppedNeedle mount by
        stripping the SABnzbd ``complete_dir`` prefix; fall back to the job-folder
        basename when the prefix doesn't match."""
        complete_dir = await self._complete_dir()
        remote = PurePosixPath(storage)
        if complete_dir:
            try:
                rel = remote.relative_to(PurePosixPath(complete_dir))
                return self._mount / Path(*rel.parts)
            except ValueError:
                pass
        return self._mount / remote.name

    async def _exact_local_storage(self, storage: str) -> Path | None:
        """Map cleanup evidence only when SAB's complete root is known exactly."""

        complete_dir = await self._complete_dir()
        if not complete_dir:
            return None
        try:
            relative = PurePosixPath(storage).relative_to(PurePosixPath(complete_dir))
        except ValueError:
            return None
        return self._mount / Path(*relative.parts)

    def _enumerate_audio(self, folder: Path) -> list[Path]:
        """Audio files under the finished job folder (bounded DFS), confined to the
        mount. Sync filesystem I/O - the caller offloads it off the event loop."""
        mount = self._mount.resolve()
        try:
            root = folder.resolve()
        except OSError:
            return []
        if not root.is_relative_to(mount) or not root.is_dir():
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


def _dir_has_file(folder: Path) -> bool:
    try:
        return folder.is_dir() and any(p.is_file() for p in folder.iterdir())
    except OSError:
        return False


def _queue_status(slot: SabnzbdQueueSlot) -> DownloadTaskStatus:
    state = slot.status.lower()
    mb = _to_float(slot.mb)
    mbleft = _to_float(slot.mbleft)
    bytes_total = int(mb * 1024 * 1024)
    bytes_downloaded = int(max(0.0, mb - mbleft) * 1024 * 1024)
    active = state == "downloading"
    percent = float(_to_int(slot.percentage))
    if state in _QUEUE_NOT_ACTIVE:
        status = "queued"
    elif active:
        status = "downloading"
    else:
        # Post-download (checking/verifying/repairing/extracting/moving/running): all the
        # bytes are in. SABnzbd zeroes the queue percentage during unpack, so hold the bar
        # at 100% and let status="processing" convey the phase - don't show a 0% regression.
        status = "processing"
        percent = 100.0
        bytes_downloaded = bytes_total
    return DownloadTaskStatus(
        task_id="",
        status=status,
        files_total=1,
        files_completed=0,
        bytes_total=bytes_total,
        bytes_downloaded=bytes_downloaded,
        progress_percent=percent,
        has_active_transfer=active,
        matched_transfers=1,
    )


# NOTE: a disk-full unpack failure ("…write error or disk is full?") is downgraded to a
# RETRYABLE outcome by the orchestrator (it skips the blocklist), so the fail_message is
# surfaced verbatim here and classified there - not special-cased in this mapping.
def _history_status(slot: SabnzbdHistorySlot) -> DownloadTaskStatus:
    state = slot.status.lower()
    if state == "deleted":
        # The job was removed from SABnzbd (user/cleanup) - treat as a terminal failure so
        # the orchestrator fails over instead of polling to the 6-hour deadline.
        return DownloadTaskStatus(
            task_id="", status="failed", error="job removed from SABnzbd", matched_transfers=1
        )
    if state == "completed":
        return DownloadTaskStatus(
            task_id="",
            status="completed",
            files_total=1,
            files_completed=1,
            bytes_total=slot.bytes,
            bytes_downloaded=slot.bytes,
            progress_percent=100.0,
            matched_transfers=1,
        )
    if state == "failed":
        return DownloadTaskStatus(
            task_id="",
            status="failed",
            error=slot.fail_message or "download failed",
            bytes_total=slot.bytes,
            matched_transfers=1,
        )
    # Verifying/Repairing/Extracting/Moving in history -> still post-processing, but the
    # download finished, so hold the bar at 100% instead of dropping it to 0 until the
    # job flips to Completed.
    return DownloadTaskStatus(
        task_id="",
        status="processing",
        files_total=1,
        files_completed=0,
        bytes_total=slot.bytes,
        bytes_downloaded=slot.bytes,
        progress_percent=100.0,
        matched_transfers=1,
    )


def _to_float(value: str) -> float:
    try:
        return float(str(value).strip() or 0)
    except (TypeError, ValueError):
        return 0.0


def _to_int(value: str) -> int:
    try:
        return int(float(str(value).strip() or 0))
    except (TypeError, ValueError):
        return 0
