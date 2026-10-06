"""``NzbgetDownloadClient`` - a ``DownloadClientProtocol`` impl over ``NzbgetClient``,
the second Usenet download client alongside SABnzbd (D5).

enqueue = fetch the release NZB -> validate -> ``append`` -> ``TaskHandle{job_name,
nzo_id}``. If the fetch gets no response at all (transport failure - never a content
rejection or a definitive indexer HTTP error), fall back to handing NZBGet the
enclosure URL to fetch itself, for indexers only it can reach. get_status walks
``listgroups`` then ``history``; **only the true ``DOWNLOADING`` state sets
``has_active_transfer``** (so QUEUED/PAUSED/FETCHING and the post-processing states
don't trip the orchestrator's stall/queued watchdogs). list_completed_files remaps the
job folder onto the DroppedNeedle downloads mount and enumerates the audio files (the
folder-based import source, D18). Completed materialization is inspected separately
from abort/history removal so local cleanup remains durable.

Three NZBGet-specific notes:

* ``NZBID`` is an int; it is stringified into the shared ``TaskHandle.nzo_id`` field,
  which keeps SABnzbd's name for historical reasons.
* History ``Status`` is a compound ``CATEGORY/DETAIL`` string, so the category before
  the slash is what maps onto the protocol's states.
* ``EnqueueRequest.post_processing`` is SABnzbd's ``pp`` level and has no NZBGet
  equivalent; NZBGet decides unpack/repair per category and server config, so the
  field is deliberately ignored here rather than guessed at.

No ``from __future__ import annotations`` (the conformance test compares real
signatures).
"""

import asyncio
import logging
import re
from pathlib import Path

from core.exceptions import NewznabApiError
from models.common import ServiceStatus
from repositories.download_mount import DownloadsMount, dir_has_file
from repositories.protocols.download_client import (
    DownloadMaterialization,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    TaskHandle,
)

from .nzbget_client import NzbgetApiError, NzbgetClient
from .nzbget_models import NzbgetGroup, NzbgetHistoryItem

logger = logging.getLogger(__name__)

# Queue states that move 0 bytes (NOT active transfers). FETCHING is NZBGet pulling the
# NZB from a URL, the counterpart of SABnzbd's "Grabbing". Everything else in the queue
# that isn't DOWNLOADING is post-processing -> "processing".
_GROUP_NOT_ACTIVE = {"QUEUED", "PAUSED", "FETCHING"}

# History Status categories (the part before the slash).
_HISTORY_OK = {"SUCCESS", "WARNING"}

# NZBGet config values interpolate each other (``DestDir=${MainDir}/dst``), and
# ``config`` returns them unexpanded.
_CONFIG_VAR_RE = re.compile(r"\$\{(\w+)\}")
_CONFIG_EXPAND_PASSES = 5

_CATEGORY_NAME_RE = re.compile(r"^category\d+\.name$")


class NzbgetDownloadClient:
    _DIAGNOSIS_SAMPLE = 3

    def __init__(
        self,
        client: NzbgetClient,
        url: str,
        username: str,
        password: str,
        downloads_mount: Path,
    ) -> None:
        self._client = client
        self._url = url
        self._username = username
        self._password = password
        self._mount = Path(downloads_mount)
        self._complete_dir_cache: str | None = None
        self._paths = DownloadsMount(self._mount, self._complete_dir, client_name="nzbget")

    @property
    def client_name(self) -> str:
        return "nzbget"

    def is_configured(self) -> bool:
        # NZBGet's control interface is reachable without credentials only if the admin
        # cleared them, which its own docs discourage; a URL alone is still a usable
        # config, so the password is not required here.
        return bool(self._url)

    async def health_check(self) -> ServiceStatus:
        try:
            version = await self._client.version()
        except Exception as exc:  # noqa: BLE001 - health check never raises
            return ServiceStatus(status="error", message=str(exc))
        return ServiceStatus(
            status="ok", version=version, message=f"NZBGet {version}" if version else "NZBGet"
        )

    async def get_categories(self) -> list[str]:
        """NZBGet's category names (for the settings picker). They live in the main
        config as ``Category1.Name``, ``Category2.Name``, ... rather than in a list."""
        try:
            options = await self._client.config()
        except NzbgetApiError:
            return []
        return [
            option.value
            for option in options
            if _CATEGORY_NAME_RE.match(option.name.lower()) and option.value
        ]

    async def get_complete_dir(self) -> str:
        """NZBGet's completed-downloads dir (the mount-hint shown in settings)."""
        return await self._complete_dir()

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        if not request.nzb_url:
            raise NzbgetApiError("enqueue requires an nzb_url for the usenet source")
        job_name = request.job_name or f"droppedneedle-{request.task_id}"
        try:
            nzb_bytes = await self._client.fetch_nzb(request.nzb_url)
        except NewznabApiError as exc:
            if not getattr(exc, "transport_failure", False):
                # Either a deterministic content rejection (indexer error/limit page) or a
                # definitive indexer HTTP error (401/403/429/5xx): the indexer answered, so
                # handing the same URL to NZBGet cannot help. Re-raise for the existing
                # failover/blocklist handling.
                raise
            # No-response transport failure (DNS/refused/timeout): this indexer may be
            # reachable only from the NZBGet host, so hand NZBGet the enclosure URL and let
            # it fetch the NZB itself. The URL is never logged: Newznab enclosures carry
            # the indexer's apikey.
            logger.info(
                "download.usenet_enqueue_appendurl_fallback",
                extra={"task_id": request.task_id, "job_name": job_name},
            )
            nzb_id = await self._client.append_url(
                job_name,
                request.nzb_url,
                category=request.category,
                priority=request.priority,
                dupe_key=job_name,
            )
            if nzb_id <= 0:
                raise NzbgetApiError("NZBGet rejected the NZB URL (no NZBID returned)")
            return TaskHandle(source="usenet", job_name=job_name, nzo_id=str(nzb_id))
        nzb_id = await self._client.append(
            job_name,
            nzb_bytes,
            category=request.category,
            priority=request.priority,
            dupe_key=job_name,
        )
        if nzb_id <= 0:
            raise NzbgetApiError("NZBGet rejected the NZB (no NZBID returned)")
        return TaskHandle(source="usenet", job_name=job_name, nzo_id=str(nzb_id))

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        group = await self._find_group(handle)
        if group is not None:
            return _group_status(group)
        item = await self._find_history_item(handle)
        if item is not None:
            return _history_status(item)
        # Not in the queue or history yet (just-added / between states) or gone.
        return DownloadTaskStatus(task_id="", status="queued", matched_transfers=0)

    async def abort(self, handle: TaskHandle) -> bool:
        group = await self._find_group(handle)
        if group is not None:
            # GroupFinalDelete removes the job and its incomplete bytes without leaving a
            # history record, which is what "remove only client-owned incomplete data"
            # means here.
            return await self._client.editqueue("GroupFinalDelete", [group.nzb_id])
        item = await self._find_history_item(handle)
        if item is None:
            return True
        if _history_category(item) == "FAILURE":
            return await self._client.editqueue("HistoryFinalDelete", [item.nzb_id])
        # Completed output is not client-owned temporary data. The cleanup service must
        # validate and remove its exact workspace before the history record is discarded.
        return False

    async def inspect_materialization(self, handle: TaskHandle) -> DownloadMaterialization:
        group = await self._find_group(handle)
        if group is not None:
            return DownloadMaterialization(
                state="active",
                nzo_id=str(group.nzb_id),
                mount_root=str(self._mount),
                mount_healthy=await self.downloads_mount_healthy(),
            )
        item = await self._find_history_item(handle)
        mount_healthy = await self.downloads_mount_healthy()
        if item is None:
            return DownloadMaterialization(
                state="missing", mount_root=str(self._mount), mount_healthy=mount_healthy
            )
        local = await self._paths.exact_local_storage(item.storage) if item.storage else None
        return DownloadMaterialization(
            state=("failed" if _history_category(item) == "FAILURE" else "completed"),
            nzo_id=str(item.nzb_id),
            remote_storage=item.storage,
            mount_root=str(self._mount),
            workspace_path=str(local) if local is not None else "",
            mount_healthy=mount_healthy,
        )

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        item = await self._find_history_item(handle)
        if item is None:
            if await self._find_group(handle) is not None:
                return False
            return True
        # HistoryDelete drops the record and KEEPS the files; HistoryFinalDelete removes
        # both. Completed output was already removed locally, so it must use the former.
        command = (
            "HistoryFinalDelete" if _history_category(item) == "FAILURE" else "HistoryDelete"
        )
        return await self._client.editqueue(command, [item.nzb_id])

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        item = await self._find_history_item(handle)
        if item is None or not item.storage:
            return []
        local = await self._paths.local_storage(item.storage)
        return await asyncio.to_thread(self._paths.enumerate_audio, local)

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
            history = await self._client.history()
        except Exception:  # noqa: BLE001 - a diagnostic must never raise
            return MountDiagnosis(supported=True, client_downloads_dir=client_dir or None)
        completed = [
            item for item in history if _history_category(item) in _HISTORY_OK and item.storage
        ]
        if not completed:
            return MountDiagnosis(
                supported=True,
                completed_downloads=0,
                mount_has_files=True,
                client_downloads_dir=client_dir or None,
            )
        sample = completed[: self._DIAGNOSIS_SAMPLE]
        resolvable = 0
        for item in sample:
            local = await self._paths.local_storage(item.storage)
            if await asyncio.to_thread(dir_has_file, local):
                resolvable += 1
        has_files = await asyncio.to_thread(self._paths.has_any_file)
        return MountDiagnosis(
            supported=True,
            completed_downloads=len(completed),
            mount_has_files=has_files,
            resolvable_downloads=resolvable,
            sampled_downloads=len(sample),
            client_downloads_dir=client_dir or None,
        )

    async def downloads_mount_healthy(self) -> bool:
        """Whether DroppedNeedle's downloads MOUNT itself is usable (see
        ``DownloadsMount.healthy``)."""
        return await self._paths.healthy()

    # --- internals --------------------------------------------------------------

    @staticmethod
    def _matches(nzb_id: int, name: str, dupe_key: str, handle: TaskHandle) -> bool:
        if handle.nzo_id:
            return str(nzb_id) == handle.nzo_id
        if not handle.job_name:
            return False
        # DupeKey carries the same pre-enqueue key as the name and survives the rename
        # NZBGet applies to a duplicate job, so it is checked as well.
        return handle.job_name in (name, dupe_key)

    async def _find_group(self, handle: TaskHandle) -> NzbgetGroup | None:
        groups = await self._client.listgroups()
        matches = [
            group
            for group in groups
            if self._matches(group.nzb_id, group.nzb_name, group.dupe_key, handle)
        ]
        if len(matches) > 1:
            raise NzbgetApiError("NZBGet returned an ambiguous job identity")
        return matches[0] if matches else None

    async def _find_history_item(self, handle: TaskHandle) -> NzbgetHistoryItem | None:
        # NZBGet's history has no server-side filter, so the match happens here. NZBID is
        # its unique key; the job_name/DupeKey match is the pre-enqueue crash-recovery
        # fallback, for the window where the append response never reached us.
        history = await self._client.history()
        matches = [
            item
            for item in history
            if self._matches(item.nzb_id, item.nzb_name or item.name, item.dupe_key, handle)
        ]
        if len(matches) > 1:
            raise NzbgetApiError("NZBGet returned an ambiguous job identity")
        return matches[0] if matches else None

    async def _complete_dir(self) -> str:
        # Cache only a NON-EMPTY value: a transient config failure (or an empty result)
        # must not poison the cache and force the basename-only remap forever, which would
        # mis-resolve a per-category job folder.
        if self._complete_dir_cache:
            return self._complete_dir_cache
        try:
            options = await self._client.config()
        except Exception:  # noqa: BLE001 - transient; retry next call
            return ""
        values = {option.name.lower(): option.value for option in options}
        value = _expand_config_vars(values.get("destdir", ""), values)
        if value:
            self._complete_dir_cache = value
        return value


def _expand_config_vars(value: str, values: dict[str, str]) -> str:
    """Resolve NZBGet's ``${MainDir}``-style references in a config value.

    ``config`` returns options as written in nzbget.conf, so ``DestDir`` is typically
    ``${MainDir}/dst`` and has to be expanded before it can be compared with the
    absolute paths NZBGet reports for finished jobs. Bounded passes, because a
    hand-edited config can reference itself.
    """
    for _ in range(_CONFIG_EXPAND_PASSES):
        if "${" not in value:
            break
        value = _CONFIG_VAR_RE.sub(lambda m: values.get(m.group(1).lower(), m.group(0)), value)
    return value


def _history_category(item: NzbgetHistoryItem) -> str:
    """The part of a compound history ``Status`` before the slash."""
    return item.status.split("/", 1)[0].upper()


def _group_status(group: NzbgetGroup) -> DownloadTaskStatus:
    state = group.status.upper()
    bytes_total = group.file_size
    # DownloadedSize is not reported by every NZBGet version, but FileSize and
    # RemainingSize always are, so progress is derived from them.
    bytes_downloaded = max(0, bytes_total - group.remaining_size)
    active = state == "DOWNLOADING"
    percent = (bytes_downloaded / bytes_total * 100.0) if bytes_total else 0.0
    if state in _GROUP_NOT_ACTIVE:
        status = "queued"
    elif active:
        status = "downloading"
    else:
        # Post-download (par check, repair, unpack, move, script): all the bytes are in.
        # Hold the bar at 100% and let status="processing" convey the phase.
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


# NOTE: a disk-full unpack failure is downgraded to a RETRYABLE outcome by the
# orchestrator (it skips the blocklist), so the failure detail is surfaced verbatim here
# and classified there - not special-cased in this mapping.
def _history_status(item: NzbgetHistoryItem) -> DownloadTaskStatus:
    category = _history_category(item)
    if category == "DELETED":
        # The job was removed from NZBGet (user/cleanup) - treat as a terminal failure so
        # the orchestrator fails over instead of polling to the 6-hour deadline.
        return DownloadTaskStatus(
            task_id="", status="failed", error="job removed from NZBGet", matched_transfers=1
        )
    if category in _HISTORY_OK:
        return DownloadTaskStatus(
            task_id="",
            status="completed",
            files_total=1,
            files_completed=1,
            bytes_total=item.file_size,
            bytes_downloaded=item.file_size,
            progress_percent=100.0,
            matched_transfers=1,
        )
    if category == "FAILURE":
        return DownloadTaskStatus(
            task_id="",
            status="failed",
            error=_failure_reason(item),
            bytes_total=item.file_size,
            matched_transfers=1,
        )
    # Still post-processing, but the download finished, so hold the bar at 100% instead of
    # dropping it to 0 until the job reaches a terminal category.
    return DownloadTaskStatus(
        task_id="",
        status="processing",
        files_total=1,
        files_completed=0,
        bytes_total=item.file_size,
        bytes_downloaded=item.file_size,
        progress_percent=100.0,
        matched_transfers=1,
    )


def _failure_reason(item: NzbgetHistoryItem) -> str:
    """NZBGet has no single fail_message; the useful detail is spread across the
    per-stage statuses, so report whichever stage actually failed."""
    detail = item.status.split("/", 1)[1] if "/" in item.status else ""
    stages = [
        ("par", item.par_status),
        ("unpack", item.unpack_status),
        ("move", item.move_status),
        ("script", item.script_status),
    ]
    for label, value in stages:
        upper = value.upper()
        if upper and upper not in ("SUCCESS", "NONE", "OK", "SKIPPED"):
            return f"{label} failed: {value}"
    return f"download failed: {detail}" if detail else "download failed"
