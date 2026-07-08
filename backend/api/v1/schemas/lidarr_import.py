"""Request/response DTOs for the read-only Lidarr importer (LidarrImport).

Wire format snake_case; every struct is hand-mirrored in ``frontend/src/lib/types.ts``.
The connection settings + mask live in ``schemas/settings.py`` (the PreferencesService
config pattern); everything else is here.
"""

from infrastructure.msgspec_fastapi import AppStruct


class LidarrTestResponse(AppStruct):
    """Result of testing the submitted Lidarr connection (like ``IndexerTestResponse`` minus
    ``suggested_url`` - the URL is normalised silently, so there is nothing to suggest)."""

    valid: bool
    version: str | None = None
    message: str = ""


class LidarrImportStatusResponse(AppStruct):
    """The non-admin gate for the Following-page import button. Carries ONLY the boolean -
    never the url or api_key (config-leak guard)."""

    configured: bool


class LidarrArtistCandidate(AppStruct):
    """One monitored Lidarr artist annotated for the requesting user."""

    mbid: str
    name: str
    monitor_new_items: str  # "none" | "all"
    already_following: bool
    would_auto_download: bool


class LidarrArtistListResponse(AppStruct):
    artists: list[LidarrArtistCandidate]
    total: int


class LidarrImportRequest(AppStruct):
    selected_mbids: list[str]


class LidarrImportResponse(AppStruct):
    """Import summary. ``imported`` = brand-new follows only; ``already_following`` = the
    pre-existing subset (disjoint, no double-count); ``auto_download_enabled`` = brand-new
    auto-download follows only (D9). ``approval_batch_id`` is set only for a non-admin import
    that mirrored auto-download (else null - admins are approved by role)."""

    imported: int
    already_following: int
    skipped_invalid: int
    auto_download_enabled: int
    approval_batch_id: str | None = None
