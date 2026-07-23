"""Security: the auth matrix. Every native-engine endpoint that declares an auth
posture is exercised under three identities and must reject the wrong ones.

- No auth          -> 401 (the auth dependency rejects before the body runs).
- Authenticated user -> 403 on admin-only endpoints; admitted on user endpoints.
- Admin            -> admitted everywhere.

"Admitted" means "auth did not reject" (status not in {401, 403}); the body itself
runs against benign service mocks, so a 200/404/422/500 from the mock all count as
auth-passed. This test owns the auth posture; route unit tests own body behaviour.

Service providers are overridden with non-raising mocks so dependency resolution
never 500s before the auth dependency is evaluated (which would mask a 401).
SSE stream endpoints are covered separately in ``test_sse_auth.py`` (their infinite
generators can't be driven through ``TestClient`` for the admitted case).
"""

from unittest.mock import AsyncMock

from fastapi import APIRouter, FastAPI, HTTPException

from api.v1.routes import connect_apps_routes
from api.v1.routes import auth as auth_routes
from api.v1.routes import download_client as download_client_routes
from api.v1.routes import download_clients as download_clients_routes
from api.v1.routes import downloads as downloads_routes
from api.v1.routes import downloads_search as downloads_search_routes
from api.v1.routes import following as following_routes
from api.v1.routes import free_music as free_music_routes
from api.v1.routes import import_drop as import_drop_routes
from api.v1.routes import plugins as plugins_routes
from api.v1.routes import library as library_routes
from api.v1.routes import library_contributions as library_contribution_routes
from api.v1.routes import library_management as library_management_routes
from api.v1.routes import library_operations_target as library_operations_target_routes
from api.v1.routes import library_policies as library_policy_routes
from api.v1.routes import library_policies_target as target_library_policy_routes
from api.v1.routes import library_scan_target as target_library_scan_routes
from api.v1.routes import library_target as target_library_routes
from api.v1.routes import lidarr_import as lidarr_import_routes
from api.v1.routes import discovery_batches as discovery_batches_routes
from api.v1.routes import library_scan as library_scan_routes
from api.v1.routes import me_connections as me_routes
from api.v1.routes import navidrome_preferences as navidrome_preferences_routes
from api.v1.routes import playlists as playlists_routes
from api.v1.routes import quarantine as quarantine_routes
from api.v1.routes import requests_page as requests_page_routes
from api.v1.routes import settings as settings_routes
from api.v1.routes import spotify as spotify_routes
from api.v1.routes import stream as stream_routes
from api.v1.routes import system as system_routes
from api.v1.routes import tracks as tracks_routes
from core.dependencies import (
    get_app_password_service,
    get_auth_service,
    get_auth_store,
    get_cache,
    get_discovery_batch_service,
    get_download_client_repository,
    get_download_service,
    get_download_store,
    get_events_service,
    get_follow_service,
    get_geocoding_repository,
    get_jellyfin_playback_service,
    get_jellyfin_user_auth_service,
    get_lastfm_auth_service,
    get_library_manager,
    get_library_management_preview_service,
    get_library_management_profile_service,
    get_library_contribution_service,
    get_library_policy_service,
    get_library_scanner,
    get_library_service,
    get_local_files_service,
    get_drop_import_service,
    get_free_music_service,
    get_plugin_host,
    get_lidarr_import_repository,
    get_lidarr_import_service,
    get_navidrome_playback_service,
    get_navidrome_folder_scope_service,
    get_now_playing_service,
    get_per_user_client_factory,
    get_personal_mix_service,
    get_playlist_service,
    get_plex_playback_service,
    get_plex_user_auth_service,
    get_preferences_service,
    get_request_service,
    get_requests_page_service,
    get_scan_state_store,
    get_settings_service,
    get_spotify_import_service,
    get_sse_publisher,
    get_user_connections_store,
    get_user_listening_prefs_store,
    get_user_section_prefs_store,
    get_wanted_watcher_service,
    get_target_catalog_correction_service,
    get_target_explicit_reidentification_worker,
    get_target_identity_repair_service,
    get_target_library_diagnostics_service,
    get_target_identification_queue,
    get_target_library_operation_service,
    get_target_library_review_service,
    get_target_library_scan_coordinator,
    get_target_native_library_service,
    get_target_catalog_writer_service,
    get_cached_local_artwork_service,
    get_target_reidentification_service,
    get_library_policy_resolver,
)
from core.dependencies.service_providers import get_target_library_policy_service
from middleware import _get_current_admin, _get_current_curator, _get_current_user
from tests.helpers import build_test_client, mock_admin_user, mock_user

_SERVICE_PROVIDERS = (
    get_app_password_service,
    get_auth_service,
    get_auth_store,
    get_cache,
    get_discovery_batch_service,
    get_download_client_repository,
    get_download_service,
    get_download_store,
    get_events_service,
    get_follow_service,
    get_geocoding_repository,
    get_jellyfin_playback_service,
    get_jellyfin_user_auth_service,
    get_lastfm_auth_service,
    get_library_manager,
    get_library_management_preview_service,
    get_library_management_profile_service,
    get_library_policy_service,
    get_target_library_policy_service,
    get_library_scanner,
    get_library_service,
    get_local_files_service,
    get_drop_import_service,
    get_free_music_service,
    get_plugin_host,
    get_lidarr_import_repository,
    get_lidarr_import_service,
    get_navidrome_playback_service,
    get_navidrome_folder_scope_service,
    get_now_playing_service,
    get_per_user_client_factory,
    get_personal_mix_service,
    get_playlist_service,
    get_plex_playback_service,
    get_plex_user_auth_service,
    get_preferences_service,
    get_request_service,
    get_requests_page_service,
    get_scan_state_store,
    get_settings_service,
    get_spotify_import_service,
    get_sse_publisher,
    get_user_connections_store,
    get_user_listening_prefs_store,
    get_user_section_prefs_store,
    get_wanted_watcher_service,
    get_target_catalog_correction_service,
    get_target_explicit_reidentification_worker,
    get_target_identity_repair_service,
    get_target_library_diagnostics_service,
    get_target_identification_queue,
    get_target_library_operation_service,
    get_target_library_review_service,
    get_target_library_scan_coordinator,
    get_target_native_library_service,
    get_target_catalog_writer_service,
    get_target_reidentification_service,
    get_library_policy_resolver,
    get_cached_local_artwork_service,
    get_library_contribution_service,
)

# (method, path, body-or-None). Path params use dummy values; bodies are valid so
# body-validation never preempts the auth check with a 422.
_ADMIN_ENDPOINTS = [
    (
        "POST",
        "/api/v1/library/albums/album-1/contributions",
        {},
    ),
    (
        "PUT",
        "/api/v1/library/contributions/contribution-1/draft",
        {"expected_row_revision": 1, "draft": {}},
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/rebuild",
        {"expected_row_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/cancel",
        {"expected_row_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/discogs/search",
        {"query": "Album"},
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/discogs/select",
        {"expected_row_revision": 1, "release_id_or_url": "123"},
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/discogs/remove",
        {"expected_row_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/musicbrainz/duplicates",
        {"expected_row_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/musicbrainz/attach",
        {
            "expected_row_revision": 1,
            "release_mbid": "11111111-1111-4111-8111-111111111111",
        },
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/musicbrainz/seed",
        {"expected_row_revision": 1},
    ),
    (
        "PUT",
        "/api/v1/library/contributions/contribution-1/musicbrainz/result",
        {
            "expected_row_revision": 1,
            "release_id_or_url": "11111111-1111-4111-8111-111111111111",
        },
    ),
    (
        "POST",
        "/api/v1/library/contributions/contribution-1/musicbrainz/verify",
        {"expected_row_revision": 1},
    ),
    ("POST", "/api/v1/auth/admin/users/user-1/password-recovery", None),
    # Connect Apps admin oversight: see/revoke every user's app-passwords.
    ("GET", "/api/v1/connect-apps/admin/app-passwords", None),
    ("DELETE", "/api/v1/connect-apps/admin/app-passwords/ap-1", None),
    ("POST", "/api/v1/library/scan/start", None),
    ("POST", "/api/v1/library/scan/cancel", None),
    ("GET", "/api/v1/library/scan/unmatched", None),
    ("GET", "/api/v1/settings/library/roots", None),
    ("PUT", "/api/v1/settings/library/roots", {"library_roots": []}),
    ("GET", "/api/v1/settings/library/policy-tree", None),
    (
        "POST",
        "/api/v1/settings/library/policy-impact",
        {"settings": {"library_roots": []}},
    ),
    ("GET", "/api/v1/settings/library/path-mapping", None),
    ("GET", "/api/v1/settings/library", None),
    ("PUT", "/api/v1/settings/library", {"library_roots": []}),
    ("GET", "/api/v1/settings/library-management", None),
    (
        "PUT",
        "/api/v1/settings/library-management",
        {"settings": {}, "expected_settings_revision": "settings"},
    ),
    (
        "POST",
        "/api/v1/settings/library-management/impact",
        {"settings": {}, "expected_settings_revision": "settings"},
    ),
    (
        "POST",
        "/api/v1/settings/library-management/validate",
        {"settings": {}, "expected_settings_revision": "settings"},
    ),
    (
        "GET",
        "/api/v1/settings/library-management/profiles/profile-1",
        None,
    ),
    (
        "POST",
        "/api/v1/settings/library-management/profiles",
        {"name": "Profile", "expected_settings_revision": "settings"},
    ),
    (
        "POST",
        "/api/v1/settings/library-management/profiles/profile-1/copy",
        {"name": "Profile copy", "expected_settings_revision": "settings"},
    ),
    (
        "PUT",
        "/api/v1/settings/library-management/profiles/profile-1",
        {
            "profile": {"id": "profile-1", "name": "Profile"},
            "expected_settings_revision": "settings",
        },
    ),
    (
        "DELETE",
        "/api/v1/settings/library-management/profiles/profile-1",
        {"expected_settings_revision": "settings"},
    ),
    (
        "GET",
        "/api/v1/settings/library-management/profiles/profile-1/preset-diff",
        None,
    ),
    (
        "POST",
        "/api/v1/settings/library-management/activation-previews",
        {
            "root_id": "root-1",
            "settings": {},
            "expected_settings_revision": "settings",
            "expected_policy_revision": "policy",
        },
    ),
    (
        "GET",
        "/api/v1/settings/library-management/activation-previews/job-1",
        None,
    ),
    (
        "POST",
        "/api/v1/settings/library-management/activation-confirmations",
        {
            "settings": {},
            "proofs": [],
            "expected_settings_revision": "settings",
            "confirmation": True,
        },
    ),
    (
        "POST",
        "/api/v1/library/management/previews",
        {
            "selection": {"kind": "tracks", "ids": ["track-1"]},
            "profile_id": "profile-1",
            "expected_settings_revision": "settings",
            "expected_policy_revision": "policy",
        },
    ),
    ("GET", "/api/v1/library/management/tracks/track-1/tag-editor", None),
    (
        "POST",
        "/api/v1/library/management/tag-edit-previews",
        {
            "local_track_id": "track-1",
            "mode": "write_once",
            "expected_settings_revision": "settings",
            "expected_policy_revision": "policy",
            "fields": [{"field_name": "title", "value": "Title"}],
        },
    ),
    ("GET", "/api/v1/library/management/previews/job-1", None),
    ("GET", "/api/v1/library/management/previews/job-1/items", None),
    (
        "POST",
        "/api/v1/settings/library/policy-apply-preview",
        {"scope_ids": [], "expected_policy_revision": "policy"},
    ),
    ("POST", "/api/v1/library/scan/unmatched/1/resolve", {"resolution": "reject"}),
    (
        "POST",
        "/api/v1/library/scan/unmatched/resolve-batch",
        {"release_group_mbid": "rg-1", "items": []},
    ),
    ("GET", "/api/v1/download-client/config", None),
    ("PUT", "/api/v1/download-client/config", {}),
    ("POST", "/api/v1/download-client/test", {}),
    ("GET", "/api/v1/downloads/quarantine", None),
    ("DELETE", "/api/v1/downloads/quarantine/1", None),
    ("POST", "/api/v1/downloads/task-1/reimport", None),
    ("GET", "/api/v1/library/tracks/file-1/tags", None),
    ("POST", "/api/v1/library/albums/rg-1/rescan", None),
    # Spotify app credentials + home settings (admin-gated at the /settings router level).
    ("GET", "/api/v1/settings/spotify", None),
    ("PUT", "/api/v1/settings/spotify", {}),
    ("GET", "/api/v1/settings/home", None),
    ("PUT", "/api/v1/settings/home", {}),
    # Wanted watcher settings (admin, download-clients router)
    ("GET", "/api/v1/download-clients/wanted", None),
    ("PUT", "/api/v1/download-clients/wanted", {}),
    # Free Music settings (admin, settings router)
    ("GET", "/api/v1/settings/free-music", None),
    (
        "PUT",
        "/api/v1/settings/free-music",
        {"enabled": True, "preferred_format": "flac"},
    ),
    # Get it purchase-link settings (admin, settings router)
    ("GET", "/api/v1/settings/get-it", None),
    (
        "PUT",
        "/api/v1/settings/get-it",
        {"store_region": "US", "support_droppedneedle": True},
    ),
    # Upcoming Events sources (admin, settings router)
    ("GET", "/api/v1/settings/events", None),
    ("PUT", "/api/v1/settings/events", {}),
    ("POST", "/api/v1/settings/events/test-ticketmaster", {}),
    ("POST", "/api/v1/settings/events/test-skiddle", {}),
    # Lidarr import: connection config + Test are admin-only (LidarrImport).
    ("GET", "/api/v1/lidarr-import/config", None),
    ("PUT", "/api/v1/lidarr-import/config", {}),
    ("POST", "/api/v1/lidarr-import/test", {}),
    # Plugin API (phase 01b): admin-only. No source surfaces exist (D22).
    # (both reject a plain user with 403, so they live in the admin list).
    ("GET", "/api/v1/plugins", None),
    ("POST", "/api/v1/plugins/install", {"repository_url": "https://github.com/o/r"}),
    ("PUT", "/api/v1/plugins/demo", {"enabled": False, "settings": {}}),
    ("DELETE", "/api/v1/plugins/demo", None),
    # Drop importer (phase 01c): curator-gated (admin + trusted) - a plain user
    # must see 403. POST /import/uploads is multipart and can't be driven here;
    # its auth posture is covered in tests/routes/test_import_drop_routes.py.
    # Free Music (D24): cancel/retry are curator actions, so a plain user sees 403.
    ("POST", "/api/v1/free-music/tasks/t-1/cancel", None),
    ("POST", "/api/v1/free-music/tasks/t-1/retry", None),
    ("GET", "/api/v1/import/jobs", None),
    ("GET", "/api/v1/import/jobs/job-1", None),
    ("POST", "/api/v1/import/items/1/match", {"release_group_mbid": "rg-1"}),
    ("POST", "/api/v1/import/items/1/discard", None),
    # Bulk auto-download approval batches (admin, requests router)
    ("GET", "/api/v1/requests/auto-download-approval-batches", None),
    ("POST", "/api/v1/requests/auto-download-approval-batches/batch-1/approve", None),
    ("POST", "/api/v1/requests/auto-download-approval-batches/batch-1/reject", None),
    # Feedback Fixes target-only surfaces. The router remains unmounted in production
    # until the separately authorized offline replacement, but its auth contract is
    # complete and testable in isolation.
    ("GET", "/api/v1/library/reviews", None),
    ("GET", "/api/v1/library/reviews/review-1", None),
    (
        "POST",
        "/api/v1/library/reviews/review-1/keep-tagged",
        {"expected_review_revision": 1, "expected_catalog_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/reviews/review-1/detach-and-keep-tagged",
        {"expected_review_revision": 1, "expected_catalog_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/reviews/review-1/exclude",
        {"expected_review_revision": 1, "expected_catalog_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/reviews/review-1/restore",
        {"expected_review_revision": 1, "expected_catalog_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/reviews/review-1/candidate",
        {
            "expected_review_revision": 1,
            "expected_catalog_revision": 1,
            "candidate_key": "candidate-1",
        },
    ),
    (
        "POST",
        "/api/v1/library/reviews/bulk-preview",
        {"action": "keep_tagged", "selection": {"review_ids": ["review-1"]}},
    ),
    (
        "POST",
        "/api/v1/library/reviews/bulk-apply",
        {
            "preview_token": "preview-1",
            "idempotency_key": "bulk-1",
            "action": "keep_tagged",
            "selection": {"review_ids": ["review-1"]},
        },
    ),
    (
        "POST",
        "/api/v1/library/reviews/review-1/retry",
        {"expected_review_revision": 1, "expected_catalog_revision": 1},
    ),
    ("GET", "/api/v1/library/operations/job-1", None),
    ("POST", "/api/v1/library/operations/job-1/pause", {"expected_row_revision": 1}),
    ("POST", "/api/v1/library/operations/job-1/resume", {"expected_row_revision": 1}),
    ("POST", "/api/v1/library/operations/job-1/stop", {"expected_row_revision": 1}),
    (
        "POST",
        "/api/v1/library/albums/album-1/reidentify",
        {
            "expected_album_revision": 1,
            "expected_input_revision": "input-1",
            "idempotency_key": "reidentify-1",
        },
    ),
    (
        "POST",
        "/api/v1/library/operations/job-1/candidate",
        {"expected_row_revision": 1, "candidate_key": "candidate-1"},
    ),
    (
        "POST",
        "/api/v1/library/albums/album-1/split-preview",
        {"track_ids": ["track-1"], "expected_album_revisions": {"album-1": 1}},
    ),
    (
        "POST",
        "/api/v1/library/albums/album-1/split",
        {
            "track_ids": ["track-1"],
            "expected_album_revisions": {"album-1": 1},
            "preview_token": "preview-1",
            "idempotency_key": "split-1",
        },
    ),
    (
        "POST",
        "/api/v1/library/albums/merge-preview",
        {"track_ids": ["track-1"], "expected_album_revisions": {"album-1": 1}},
    ),
    (
        "POST",
        "/api/v1/library/albums/merge",
        {
            "track_ids": ["track-1"],
            "expected_album_revisions": {"album-1": 1},
            "preview_token": "preview-1",
            "idempotency_key": "merge-1",
        },
    ),
    (
        "POST",
        "/api/v1/library/tracks/move-preview",
        {
            "track_ids": ["track-1"],
            "expected_album_revisions": {"album-1": 1},
            "target_album_id": "album-2",
        },
    ),
    (
        "POST",
        "/api/v1/library/tracks/move",
        {
            "track_ids": ["track-1"],
            "expected_album_revisions": {"album-1": 1, "album-2": 1},
            "target_album_id": "album-2",
            "preview_token": "preview-1",
            "idempotency_key": "move-1",
        },
    ),
    (
        "POST",
        "/api/v1/library/albums/album-1/reset-grouping-preview",
        {"track_ids": ["track-1"], "expected_album_revisions": {"album-1": 1}},
    ),
    (
        "POST",
        "/api/v1/library/albums/album-1/reset-grouping",
        {
            "track_ids": ["track-1"],
            "expected_album_revisions": {"album-1": 1},
            "preview_token": "preview-1",
            "idempotency_key": "reset-1",
        },
    ),
    (
        "POST",
        "/api/v1/library/artists/merge-preview",
        {
            "source_artist_ids": ["artist-1"],
            "surviving_artist_id": "artist-2",
            "expected_revisions": {"artist-1": 1, "artist-2": 1},
        },
    ),
    (
        "POST",
        "/api/v1/library/artists/merge",
        {
            "source_artist_ids": ["artist-1"],
            "surviving_artist_id": "artist-2",
            "expected_revisions": {"artist-1": 1, "artist-2": 1},
            "preview_token": "preview-1",
            "idempotency_key": "artist-merge-1",
        },
    ),
    ("POST", "/api/v1/library/identity-repairs", {"idempotency_key": "repair-1"}),
    ("GET", "/api/v1/library/identity-repairs", None),
    ("GET", "/api/v1/library/identity-repairs/estimate", None),
    ("GET", "/api/v1/library/identity-repairs/job-1", None),
    ("GET", "/api/v1/library/identity-repairs/job-1/findings", None),
    (
        "POST",
        "/api/v1/library/identity-repairs/job-1/apply",
        {"expected_row_revision": 1, "confirmation": True},
    ),
    (
        "POST",
        "/api/v1/library/identity-repairs/job-1/pause",
        {"expected_row_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/identity-repairs/job-1/resume",
        {"expected_row_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/identity-repairs/job-1/stop",
        {"expected_row_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/management/identity-preparations",
        {"idempotency_key": "identity-preparation-1"},
    ),
    ("GET", "/api/v1/library/management/identity-preparations", None),
    ("GET", "/api/v1/library/management/identity-preparations/estimate", None),
    ("GET", "/api/v1/library/management/identity-preparations/job-1", None),
    (
        "GET",
        "/api/v1/library/management/identity-preparations/job-1/findings",
        None,
    ),
    (
        "POST",
        "/api/v1/library/management/identity-preparations/job-1/apply",
        {"expected_row_revision": 1, "confirmation": True},
    ),
    (
        "POST",
        "/api/v1/library/management/identity-preparations/job-1/discard",
        {"expected_row_revision": 1},
    ),
    ("GET", "/api/v1/library/scan-runs/run-1/diagnostics", None),
    ("POST", "/api/v1/library/identification/pause", {"expected_revision": 1}),
    ("POST", "/api/v1/library/identification/resume", {"expected_revision": 1}),
    (
        "POST",
        "/api/v1/library/scan-runs",
        {
            "kind": "incremental",
            "scope_ids": [],
            "expected_policy_revision": "policy",
        },
    ),
    ("GET", "/api/v1/library/scan-runs/current", None),
    ("GET", "/api/v1/library/scan-runs", None),
    ("GET", "/api/v1/library/scan-runs/estimate", None),
    ("GET", "/api/v1/library/scan-runs/run-1", None),
    (
        "POST",
        "/api/v1/library/scan-runs/run-1/pause",
        {"expected_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/scan-runs/run-1/resume",
        {"expected_revision": 1},
    ),
    (
        "POST",
        "/api/v1/library/scan-runs/run-1/stop",
        {"expected_revision": 1},
    ),
]

_USER_ENDPOINTS = [
    ("GET", "/api/v1/library/contributions/contribution-1", None),
    ("GET", "/api/v1/me/navidrome/music-folder-preferences", None),
    (
        "PUT",
        "/api/v1/me/navidrome/music-folder-preferences",
        {"mode": "all", "selected_folder_ids": []},
    ),
    ("GET", "/api/v1/library/scan/status", None),
    ("GET", "/api/v1/download-client/status", None),
    ("GET", "/api/v1/downloads", None),
    ("GET", "/api/v1/downloads/task-1/files", None),
    ("POST", "/api/v1/downloads/task-1/cancel", None),
    ("POST", "/api/v1/downloads/task-1/retry", None),
    ("GET", "/api/v1/downloads/task-1", None),
    (
        "POST",
        "/api/v1/downloads/search/album",
        {"artist_name": "A", "album_title": "B"},
    ),
    ("GET", "/api/v1/downloads/search/job-1", None),
    ("POST", "/api/v1/downloads/search/job-1/pick", {"candidate_index": 0}),
    ("POST", "/api/v1/downloads/search/job-1/dismiss", None),
    ("POST", "/api/v1/downloads/search/job-1/cancel", None),
    ("POST", "/api/v1/tracks/rec-1/request", {"artist_name": "A", "track_title": "T"}),
    ("GET", "/api/v1/library/artists", None),
    ("GET", "/api/v1/library/albums", None),
    ("GET", "/api/v1/library/tracks", None),
    ("GET", "/api/v1/library/stats", None),
    ("GET", "/api/v1/library/albums/rg-1/tracks", None),
    ("GET", "/api/v1/library/albums/rg-1/status", None),
    ("GET", "/api/v1/library/mbids", None),
    ("GET", "/api/v1/library/recently-added", None),
    ("GET", "/api/v1/library/artists/artist-1", None),
    ("GET", "/api/v1/library/artists/artist-1/albums", None),
    ("GET", "/api/v1/library/albums/album-1", None),
    ("GET", "/api/v1/library/albums/album-1/artwork/cached?v=1", None),
    ("POST", "/api/v1/library/resolve-tracks", {"items": []}),
    ("GET", "/api/v1/library/activity", None),
    ("GET", "/api/v1/me/section-prefs", None),
    ("POST", "/api/v1/me/personal-mix/refresh", None),
    ("PUT", "/api/v1/me/section-prefs", {"page": "home", "sections": []}),
    ("GET", "/api/v1/discover/batches", None),
    (
        "POST",
        "/api/v1/discover/batches",
        {"name": "b", "items": [{"release_group_mbid": "rg-1"}]},
    ),
    ("GET", "/api/v1/discover/batches/b-1", None),
    ("DELETE", "/api/v1/discover/batches/b-1", None),
    ("GET", "/api/v1/system/health", None),
    # Spotify per-user linking + browsing, and request-missing on an owned playlist.
    # (POST /me/spotify/playlists/{id}/import is intentionally omitted: it spawns a real
    # background task through the DI getters that can't be driven by the mock harness; it
    # shares the same CurrentUserDep gate as GET /me/spotify/playlists, covered here.)
    ("GET", "/api/v1/me/connections/spotify/auth/url", None),
    ("GET", "/api/v1/me/spotify/playlists", None),
    ("POST", "/api/v1/playlists/pl-1/request-missing", None),
    # Following hub. GET /following/events is omitted: it's an SSE stream whose
    # infinite generator can't be driven through TestClient for the admitted case.
    ("GET", "/api/v1/following/artists", None),
    ("GET", "/api/v1/following/new-releases", None),
    ("GET", "/api/v1/following/new-releases/recent", None),
    ("GET", "/api/v1/following/new-releases/unseen-count", None),
    ("POST", "/api/v1/following/new-releases/seen", None),
    # Upcoming Events (concerts) - per-user resources on the following router
    ("GET", "/api/v1/following/concerts", None),
    ("GET", "/api/v1/following/concerts/cities", None),
    ("PUT", "/api/v1/following/concerts/cities", {"items": []}),
    ("GET", "/api/v1/following/concerts/city-search?q=liverpool", None),
    ("GET", "/api/v1/following/concerts/unseen-count", None),
    ("POST", "/api/v1/following/concerts/seen", None),
    # Wanted watches (requests page). Ownership (403 non-owner) is covered by
    # tests/routes/test_wanted_routes.py; here: 401 unauth / user admitted.
    ("GET", "/api/v1/requests/wanted", None),
    ("POST", "/api/v1/requests/wanted/22222222-2222-2222-2222-222222222222/stop", None),
    (
        "POST",
        "/api/v1/requests/wanted/22222222-2222-2222-2222-222222222222/resume",
        None,
    ),
    ("POST", "/api/v1/requests/wanted/22222222-2222-2222-2222-222222222222/seen", None),
    # Lidarr import: any authenticated user reads candidates + imports into their OWN
    # follows (no target-user param - the caller can only ever import to themselves).
    # Free Music: reading your own downloads is a user surface.
    ("GET", "/api/v1/free-music/tasks", None),
    ("GET", "/api/v1/free-music/tasks/t-1", None),
    ("DELETE", "/api/v1/free-music/tasks", None),
    ("DELETE", "/api/v1/free-music/tasks/t-1", None),
    ("GET", "/api/v1/lidarr-import/status", None),
    ("GET", "/api/v1/lidarr-import/artists", None),
    ("POST", "/api/v1/lidarr-import/import", {"selected_mbids": []}),
    # Media-server playback attribution (issue #138): the POST reporting routes
    # carry CurrentUserDep so scrobbles/sessions land on the caller's own
    # upstream account. GET/HEAD stream proxies stay dependency-free (guarded by
    # AuthMiddleware in production, which this harness doesn't mount).
    ("POST", "/api/v1/stream/jellyfin/item-1/start", None),
    (
        "POST",
        "/api/v1/stream/jellyfin/item-1/progress",
        {"play_session_id": "s-1", "position_seconds": 1.0, "is_paused": False},
    ),
    (
        "POST",
        "/api/v1/stream/jellyfin/item-1/stop",
        {"play_session_id": "s-1", "position_seconds": 1.0},
    ),
    ("POST", "/api/v1/stream/navidrome/item-1/scrobble", None),
    ("POST", "/api/v1/stream/navidrome/item-1/now-playing", None),
    ("POST", "/api/v1/stream/navidrome/item-1/stopped", None),
    ("POST", "/api/v1/stream/plex/rk-1/scrobble", None),
    ("POST", "/api/v1/stream/plex/rk-1/now-playing", None),
    ("POST", "/api/v1/stream/plex/rk-1/stopped", None),
    # Media-server per-user account linking (issue #138)
    ("PUT", "/api/v1/me/connections/navidrome", {"username": "u", "password": "p"}),
    ("PUT", "/api/v1/me/connections/jellyfin", {"username": "u", "password": "p"}),
    ("POST", "/api/v1/me/connections/plex/auth/pin", None),
    ("GET", "/api/v1/me/connections/plex/auth/poll?pin_id=1", None),
]

_ALL_ENDPOINTS = _ADMIN_ENDPOINTS + _USER_ENDPOINTS


def _deny_admin():
    raise HTTPException(status_code=403, detail="Admin access required")


def _client(scenario: str):
    """scenario in {"none", "user", "admin"}."""
    app = FastAPI()
    v1 = APIRouter(prefix="/api/v1")
    # Registration order mirrors main.py: literal /downloads/{quarantine,search}/*
    # routers MUST precede the /downloads/{task_id} catch-all.
    for router in (
        auth_routes.router,
        library_scan_routes.router,
        download_client_routes.router,
        download_clients_routes.router,
        quarantine_routes.router,
        downloads_search_routes.router,
        downloads_routes.router,
        following_routes.router,
        tracks_routes.router,
        target_library_routes.router,
        library_contribution_routes.router,
        target_library_scan_routes.router,
        library_routes.router,
        library_operations_target_routes.router,
        target_library_policy_routes.router,
        library_management_routes.router,
        library_policy_routes.router,
        me_routes.router,
        navidrome_preferences_routes.router,
        connect_apps_routes.router,
        discovery_batches_routes.router,
        system_routes.router,
        playlists_routes.router,
        requests_page_routes.router,
        lidarr_import_routes.router,
        import_drop_routes.router,
        free_music_routes.router,
        plugins_routes.router,
        settings_routes.router,
        spotify_routes.router,
        stream_routes.router,
    ):
        v1.include_router(router)
    app.include_router(v1)

    for provider in _SERVICE_PROVIDERS:
        app.dependency_overrides[provider] = lambda: AsyncMock()
    target_native = AsyncMock()
    target_native.artists.return_value = ([], 0)
    target_native.albums.return_value = ([], 0)
    target_native.tracks.return_value = ([], 0)
    target_native.stats.return_value = {
        "total_albums": 0,
        "total_artists": 0,
        "total_tracks": 0,
        "total_size_bytes": 0,
        "format_breakdown": {},
        "review_count": 0,
        "last_scan_at": None,
    }
    target_native.provider_ids.return_value = {"musicbrainz_release_group_ids": []}
    target_native.recently_added.return_value = []
    target_native.canonical_id.return_value = None
    target_native.artist.return_value = None
    target_native.artist_albums.return_value = []
    target_native.album_detail.return_value = None
    target_native.resolve_tracks.return_value = {"items": []}
    target_native.album_tracks.return_value = []
    app.dependency_overrides[get_target_native_library_service] = lambda: target_native

    if scenario == "user":
        app.dependency_overrides[_get_current_user] = lambda: mock_user(
            role="user", user_id="user-1"
        )
        app.dependency_overrides[_get_current_admin] = _deny_admin
        # curator endpoints reject a plain user exactly like admin ones (403)
        app.dependency_overrides[_get_current_curator] = _deny_admin
    elif scenario == "admin":
        app.dependency_overrides[_get_current_user] = mock_admin_user
        app.dependency_overrides[_get_current_admin] = mock_admin_user
        app.dependency_overrides[_get_current_curator] = mock_admin_user
    # "none": no auth overrides -> real deps read request.state.user (unset) -> 401
    return build_test_client(app)


def _send(client, method: str, path: str, body):
    if body is None:
        return client.request(method, path)
    return client.request(method, path, json=body)


def test_every_endpoint_rejects_unauthenticated():
    client = _client("none")
    failures = []
    for method, path, body in _ALL_ENDPOINTS:
        status = _send(client, method, path, body).status_code
        if status != 401:
            failures.append(f"{method} {path} -> {status} (expected 401)")
    assert not failures, "unauthenticated requests not rejected:\n" + "\n".join(
        failures
    )


def test_admin_endpoints_forbid_regular_users():
    client = _client("user")
    failures = []
    for method, path, body in _ADMIN_ENDPOINTS:
        status = _send(client, method, path, body).status_code
        if status != 403:
            failures.append(f"{method} {path} -> {status} (expected 403)")
    assert not failures, "admin endpoints not forbidden to users:\n" + "\n".join(
        failures
    )


def test_user_endpoints_admit_regular_users():
    client = _client("user")
    failures = []
    for method, path, body in _USER_ENDPOINTS:
        status = _send(client, method, path, body).status_code
        if status in (401, 403):
            failures.append(
                f"{method} {path} -> {status} (auth rejected an allowed user)"
            )
    assert not failures, "user endpoints wrongly rejected a user:\n" + "\n".join(
        failures
    )


def test_admin_admitted_everywhere():
    client = _client("admin")
    failures = []
    for method, path, body in _ALL_ENDPOINTS:
        status = _send(client, method, path, body).status_code
        if status in (401, 403):
            failures.append(f"{method} {path} -> {status} (auth rejected admin)")
    assert not failures, "admin wrongly rejected:\n" + "\n".join(failures)
