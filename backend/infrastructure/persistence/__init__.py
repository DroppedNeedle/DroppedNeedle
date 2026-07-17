"""Durable persistence layer - data that survives cache clears.

All stores share a single SQLite database via :class:`PersistenceBase`.
"""

from infrastructure.persistence._database import PersistenceBase
from infrastructure.persistence.auth_store import AuthStore
from infrastructure.persistence.discovery_snapshot_store import DiscoverySnapshotStore
from infrastructure.persistence.genre_index import GenreIndex
from infrastructure.persistence.library_db import LibraryDB
from infrastructure.persistence.mbid_store import MBIDStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.persistence.request_history import RequestHistoryStore
from infrastructure.persistence.scan_state_store import ScanStateStore
from infrastructure.persistence.sync_state_store import SyncStateStore
from infrastructure.persistence.youtube_store import YouTubeStore

__all__ = [
    "PersistenceBase",
    "AuthStore",
    "DiscoverySnapshotStore",
    "GenreIndex",
    "LibraryDB",
    "MBIDStore",
    "NativeLibraryStore",
    "RequestHistoryStore",
    "ScanStateStore",
    "SyncStateStore",
    "YouTubeStore",
]
