"""Read-only Lidarr *import* client (LidarrImport feature).

This is a NEW module, deliberately separate from the deleted/tombstoned
``repositories/lidarr/`` (D8, DR1): a narrow, read-only migration aid that turns a
user's Lidarr monitored artists into DroppedNeedle follows. It does NOT resurrect the
old Lidarr management integration.
"""

from .lidarr_import_repository import LidarrImportRepository

__all__ = ["LidarrImportRepository"]
