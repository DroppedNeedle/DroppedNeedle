"""msgspec models for the NZBGet JSON-RPC API.

Modelled on the NZBGet API reference (``listgroups`` / ``history`` / ``config``).
Two shape differences from SABnzbd drive most of this file:

* **Keys are PascalCase** (``NZBID``, ``FileSizeLo``), so every field carries an
  explicit ``msgspec.field(name=...)``. NZBGet's capitalisation is not a
  mechanical transform of the python name (``NZBID``, not ``NzbId``), so a
  struct-wide ``rename`` would not reproduce it.
* **64-bit sizes arrive split into two 32-bit halves** (``FileSizeLo`` +
  ``FileSizeHi``). ``combine_size`` reassembles them. The ``*MB`` fields NZBGet
  also returns are lossy and are deliberately not used.

``NZBID`` is an **integer** (SABnzbd's ``nzo_id`` is a UUID string). It is
stringified at the repository boundary so it can travel in the shared
``TaskHandle.nzo_id`` field.
"""

import msgspec

from infrastructure.msgspec_fastapi import AppStruct


def combine_size(low: int, high: int) -> int:
    """Reassemble a NZBGet 64-bit size from its Lo/Hi 32-bit halves."""
    return (high << 32) + low


class NzbgetGroup(AppStruct):
    """An entry from ``listgroups``: a queued, downloading or post-processing job.

    ``NZBName`` is the job name without the ``.nzb`` suffix, so it carries the
    ``droppedneedle-{task_id}`` correlation key. ``DupeKey`` carries the same value
    as a second handle, because NZBGet renames a duplicate job but preserves it.
    ``RemainingSize`` is what is still to download; subtracting it from
    ``FileSize`` is the only progress figure available on every NZBGet version.
    """

    nzb_id: int = msgspec.field(name="NZBID", default=0)
    nzb_name: str = msgspec.field(name="NZBName", default="")
    nzb_nicename: str = msgspec.field(name="NZBNicename", default="")
    kind: str = msgspec.field(name="Kind", default="")
    status: str = msgspec.field(name="Status", default="")
    category: str = msgspec.field(name="Category", default="")
    dest_dir: str = msgspec.field(name="DestDir", default="")
    final_dir: str = msgspec.field(name="FinalDir", default="")
    dupe_key: str = msgspec.field(name="DupeKey", default="")
    active_downloads: int = msgspec.field(name="ActiveDownloads", default=0)
    file_size_lo: int = msgspec.field(name="FileSizeLo", default=0)
    file_size_hi: int = msgspec.field(name="FileSizeHi", default=0)
    remaining_size_lo: int = msgspec.field(name="RemainingSizeLo", default=0)
    remaining_size_hi: int = msgspec.field(name="RemainingSizeHi", default=0)

    @property
    def file_size(self) -> int:
        return combine_size(self.file_size_lo, self.file_size_hi)

    @property
    def remaining_size(self) -> int:
        return combine_size(self.remaining_size_lo, self.remaining_size_hi)


class NzbgetHistoryItem(AppStruct):
    """An entry from ``history``: a finished, failed or deleted job.

    ``Status`` is a compound ``CATEGORY/DETAIL`` string (``SUCCESS/ALL``,
    ``FAILURE/UNPACK``, ``DELETED/MANUAL``); the repository splits on ``/``.
    ``FinalDir`` is set only when a post-processing script moved the output, so the
    job folder is ``FinalDir or DestDir``.
    """

    nzb_id: int = msgspec.field(name="NZBID", default=0)
    name: str = msgspec.field(name="Name", default="")
    nzb_name: str = msgspec.field(name="NZBName", default="")
    kind: str = msgspec.field(name="Kind", default="")
    status: str = msgspec.field(name="Status", default="")
    category: str = msgspec.field(name="Category", default="")
    dest_dir: str = msgspec.field(name="DestDir", default="")
    final_dir: str = msgspec.field(name="FinalDir", default="")
    dupe_key: str = msgspec.field(name="DupeKey", default="")
    par_status: str = msgspec.field(name="ParStatus", default="")
    unpack_status: str = msgspec.field(name="UnpackStatus", default="")
    move_status: str = msgspec.field(name="MoveStatus", default="")
    script_status: str = msgspec.field(name="ScriptStatus", default="")
    delete_status: str = msgspec.field(name="DeleteStatus", default="")
    file_size_lo: int = msgspec.field(name="FileSizeLo", default=0)
    file_size_hi: int = msgspec.field(name="FileSizeHi", default=0)
    history_time: int = msgspec.field(name="HistoryTime", default=0)

    @property
    def file_size(self) -> int:
        return combine_size(self.file_size_lo, self.file_size_hi)

    @property
    def storage(self) -> str:
        """The job's output folder, after any post-processing move."""
        return self.final_dir or self.dest_dir


class NzbgetConfigOption(AppStruct):
    """One ``{"Name": ..., "Value": ...}`` pair from ``config``."""

    name: str = msgspec.field(name="Name", default="")
    value: str = msgspec.field(name="Value", default="")
