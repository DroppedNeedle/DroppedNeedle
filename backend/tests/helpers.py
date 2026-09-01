"""Shared test helpers for observability / log field assertions."""

from __future__ import annotations

import logging
import shutil
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.exceptions import RequestValidationError
from fastapi.testclient import TestClient
from starlette.exceptions import HTTPException as StarletteHTTPException

from core.exception_handlers import (
    circuit_open_error_handler,
    client_disconnected_handler,
    configuration_error_handler,
    automatic_management_hold_handler,
    external_service_error_handler,
    general_exception_handler,
    http_exception_handler,
    request_validation_error_handler,
    resource_not_found_handler,
    source_resolution_error_handler,
    starlette_http_exception_handler,
    revision_overflow_error_handler,
    stale_revision_error_handler,
    validation_error_handler,
)
from core.exception_handlers import (
    conflict_error_handler,
    permission_denied_handler,
)
from core.exceptions import (
    ClientDisconnectedError,
    ConfigurationError,
    AutomaticManagementHoldError,
    ConflictError,
    ExternalServiceError,
    PermissionDeniedError,
    ResourceNotFoundError,
    RevisionOverflowError,
    SourceResolutionError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.resilience.retry import CircuitOpenError
from infrastructure.persistence.auth_store import UserRecord
from middleware import _get_current_admin, _get_current_user
from models.library_management import LibraryManagementImportResult


def mock_admin_user() -> UserRecord:
    """A fake authenticated admin user for overriding admin-gated routes in tests."""
    return UserRecord(
        id="test-admin-id",
        display_name="Test Admin",
        role="admin",
        created_at="2024-01-01T00:00:00Z",
    )


def mock_user(role: str = "user", user_id: str = "test-user-id") -> UserRecord:
    """A fake authenticated user (default non-admin) for ownership tests."""
    return UserRecord(
        id=user_id,
        display_name="Test User",
        role=role,
        created_at="2024-01-01T00:00:00Z",
    )


def override_admin_auth(app: FastAPI) -> None:
    """Bypass the admin-auth dependency for routers gated by `_get_current_admin`."""
    app.dependency_overrides[_get_current_admin] = mock_admin_user


def override_user_auth(
    app: FastAPI, role: str = "user", user_id: str = "test-user-id"
) -> None:
    """Bypass the user-auth dependency (`_get_current_user`) with a chosen role/id.

    Used by the request-ownership tests (403 vs 200) - the route resolves
    ``CurrentUserDep`` via ``_get_current_user``.
    """
    app.dependency_overrides[_get_current_user] = lambda: mock_user(
        role=role, user_id=user_id
    )


def add_production_exception_handlers(app: FastAPI) -> FastAPI:
    app.add_exception_handler(ClientDisconnectedError, client_disconnected_handler)
    app.add_exception_handler(ResourceNotFoundError, resource_not_found_handler)
    app.add_exception_handler(ExternalServiceError, external_service_error_handler)
    app.add_exception_handler(CircuitOpenError, circuit_open_error_handler)
    app.add_exception_handler(ValidationError, validation_error_handler)
    app.add_exception_handler(ConfigurationError, configuration_error_handler)
    app.add_exception_handler(
        AutomaticManagementHoldError, automatic_management_hold_handler
    )
    app.add_exception_handler(PermissionDeniedError, permission_denied_handler)
    app.add_exception_handler(ConflictError, conflict_error_handler)
    app.add_exception_handler(StaleRevisionError, stale_revision_error_handler)
    app.add_exception_handler(RevisionOverflowError, revision_overflow_error_handler)
    app.add_exception_handler(SourceResolutionError, source_resolution_error_handler)
    app.add_exception_handler(HTTPException, http_exception_handler)
    app.add_exception_handler(StarletteHTTPException, starlette_http_exception_handler)
    app.add_exception_handler(RequestValidationError, request_validation_error_handler)
    app.add_exception_handler(Exception, general_exception_handler)
    return app


def build_test_client(app: FastAPI) -> TestClient:
    add_production_exception_handlers(app)
    return TestClient(app, raise_server_exceptions=False)


def make_test_import_publisher(library_manager, roots: dict[str, Path]):  # noqa: ANN001
    """Test double for FileProcessor's shared publication boundary.

    The durable publisher has its own real-SQLite/crash-matrix suite. Older
    acquisition integration tests use this small boundary double so they can keep
    testing orchestration without restoring the deleted direct finalizer.
    """

    def write_flac_tags(path: Path, tag) -> None:  # noqa: ANN001
        import mutagen
        from mutagen.flac import FLAC

        try:
            audio = FLAC(path)
        except mutagen.MutagenError:
            return
        values = {
            "TITLE": tag.title,
            "ARTIST": tag.artist,
            "ALBUM": tag.album,
            "ALBUMARTIST": tag.album_artist,
            "TRACKNUMBER": (
                str(tag.track_number) if tag.track_number is not None else None
            ),
            "DISCNUMBER": str(tag.disc_number) if tag.disc_number is not None else None,
            "DATE": str(tag.year) if tag.year is not None else None,
            "MUSICBRAINZ_RELEASEGROUPID": tag.musicbrainz_release_group_id,
            "MUSICBRAINZ_ALBUMID": tag.musicbrainz_release_id,
            "MUSICBRAINZ_TRACKID": tag.musicbrainz_recording_id,
            "MUSICBRAINZ_ALBUMARTISTID": tag.musicbrainz_album_artist_id,
        }
        for key, value in values.items():
            if value is None:
                audio.pop(key, None)
            else:
                audio[key] = value
        audio.save()

    async def publish(bundle):  # noqa: ANN001, ANN202
        paths: list[str] = []
        local_track_ids: list[str] = []
        for request in bundle.files:
            root = roots[request.destination_root_id]
            source = Path(request.input_path)
            destination = root / request.destination_relative_path
            destination.parent.mkdir(parents=True, exist_ok=True)
            if request.replacement_relative_path is not None:
                replacement_root = roots[str(request.replacement_root_id)]
                replacement = replacement_root / request.replacement_relative_path
                recycle_root = Path(str(request.recycle_bin_path))
                recycle_target = (
                    recycle_root
                    / str(request.replacement_local_track_id)
                    / replacement.name
                )
                recycle_target.parent.mkdir(parents=True, exist_ok=True)
                shutil.move(str(replacement), str(recycle_target))
                await library_manager.soft_delete_file(str(replacement))
            if destination.exists():
                raise FileExistsError(destination)
            shutil.move(str(source), str(destination))
            write_flac_tags(destination, request.tag)
            local_track_ids.append(
                await library_manager.upsert_file(
                    destination,
                    request.tag,
                    request.info,
                    release_group_mbid=request.release_group_mbid,
                    release_mbid=request.release_mbid,
                    recording_mbid=request.recording_mbid,
                    confidence=request.confidence,
                    source=request.source,
                    download_task_id=request.download_task_id,
                    source_path=request.source_path,
                    file_mtime=request.file_mtime,
                )
            )
            paths.append(str(destination))
        return LibraryManagementImportResult(
            bundle_id="test-import-bundle",
            paths=tuple(paths),
            local_track_ids=tuple(local_track_ids),
        )

    return publish


def make_builtin_dispatcher(get_download_service):
    """An AcquisitionDispatcher wired so a configured download client always wins:
    it forwards request_album/request_track straight to get_download_service. Lets
    tests that predate Free Music assert download dispatch unchanged."""
    from types import SimpleNamespace
    from unittest.mock import MagicMock

    from services.acquisition_dispatcher import AcquisitionDispatcher

    free_music = MagicMock()
    free_music.is_ready = MagicMock(return_value=False)
    return AcquisitionDispatcher(
        get_download_service=get_download_service,
        get_free_music_service=lambda: free_music,
        preferences_service=SimpleNamespace(is_builtin_download_ready=lambda: True),
    )


def assert_log_fields(
    records: list[logging.LogRecord],
    prefix: str,
    required_fields: list[str],
    *,
    min_count: int = 1,
) -> list[str]:
    """Assert that log records matching *prefix* contain all *required_fields*.

    Returns the matching messages for further inspection.

    Parameters
    ----------
    records:
        ``caplog.records`` or equivalent list of ``LogRecord``.
    prefix:
        The log message prefix to filter on (e.g. ``"audiodb.cache"``).
    required_fields:
        Key names that must appear as ``key=`` in every matching message.
    min_count:
        Minimum number of matching records expected (default 1).
    """
    matching = [r.message for r in records if r.message.startswith(prefix)]
    assert len(matching) >= min_count, (
        f"Expected >= {min_count} log(s) starting with '{prefix}', found {len(matching)}"
    )
    for msg in matching:
        for field in required_fields:
            assert f"{field}=" in msg, f"Field '{field}=' missing in log: {msg}"
    return matching
