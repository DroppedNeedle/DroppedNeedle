"""Domain models for the drop importer (Store Sync phase 01c).

A *job* is one upload gesture (a zip, several zips, loose files). Extraction
splits it into *items*, one per top-level folder - the album-shaped unit the
identifier works on. Items are terminal at ``imported``/``skipped``/``failed``/
``discarded``; ``needs_review`` items keep their staged files on disk until the
user matches them to a release group or discards them.
"""

from infrastructure.msgspec_fastapi import AppStruct


class JobStatus:
    PROCESSING = "processing"
    COMPLETED = "completed"
    FAILED = "failed"


class ItemStatus:
    PROCESSING = "processing"
    IMPORTED = "imported"
    SKIPPED = "skipped"
    NEEDS_REVIEW = "needs_review"
    FAILED = "failed"
    DISCARDED = "discarded"


class DropImportItem(AppStruct):
    id: int
    job_id: str
    folder_name: str
    status: str
    updated_at: float
    release_group_mbid: str | None = None
    album_title: str | None = None
    artist_name: str | None = None
    files_total: int = 0
    files_imported: int = 0
    detail: str | None = None
    staging_paths: list[str] = []


class DropImportJob(AppStruct):
    id: str
    user_id: str
    user_name: str
    status: str
    created_at: float
    upload_name: str
    staging_dir: str
    error: str | None = None
    items: list[DropImportItem] = []
