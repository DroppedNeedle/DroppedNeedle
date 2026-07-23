"""Durable acquisition-attempt and source-cleanup journal models."""

from infrastructure.msgspec_fastapi import AppStruct
from repositories.protocols.download_client import TaskHandle


class DownloadAttempt(AppStruct):
    """One candidate hand-off, retained independently of the queue task.

    ``handle`` is reconstructed from the store's house-codec JSON column. Paths are
    internal cleanup evidence only and are never exposed through an API response.
    """

    id: str
    task_id: str
    source: str
    candidate_index: int
    job_name: str
    handle: TaskHandle
    state: str
    disposition: str
    remote_storage: str | None = None
    mount_root: str | None = None
    workspace_path: str | None = None
    materialized_paths: list[str] = []
    publisher_bundle_ids: list[str] = []
    legacy_reconciled: bool = False
    cleanup_failures: int = 0
    next_retry_at: float = 0.0
    lease_owner: str | None = None
    lease_expires_at: float | None = None
    error_code: str | None = None
    created_at: float = 0.0
    updated_at: float = 0.0
    completed_at: float | None = None
    row_revision: int = 1


class DownloadCleanupReconciliation(AppStruct):
    """Resumable breadth-first scan position for one SABnzbd mount."""

    mount_key: str
    mount_root: str
    pending_directories: list[str]
    current_directory: str | None = None
    last_entry: str | None = None
    completed: bool = False
    updated_at: float = 0.0
