"""The production target application must expose the same /api/v1 surface as the
normal app, except where it deliberately swaps in a target-specific variant.

This pins the invariant that was violated when the fork's prowlarr/qbittorrent
routers were registered in main.py but not in target_application.py: the first
boot after a library upgrade serves target_main:app, so those endpoints 404ed
until the container was restarted.
"""

from fastapi import FastAPI

# Operations the target application is expected NOT to serve. Each entry is an
# intentional divergence, not an oversight - add only with a reason.
#
# library_target/library_scan_target replace the normal library routes with
# target-specific variants: they key albums by {album_id} rather than {mbid}
# and expose /library/scan-runs/* in place of /library/scan/*.
_LIBRARY_TARGET_VARIANTS = frozenset(
    {
        ("DELETE", "/api/v1/library/album/{album_mbid}"),
        ("DELETE", "/api/v1/library/tracks/{file_id}"),
        ("GET", "/api/v1/library/"),
        ("GET", "/api/v1/library/albums/{mbid}/status"),
        ("GET", "/api/v1/library/albums/{mbid}/tracks"),
        ("GET", "/api/v1/library/grouped"),
        ("GET", "/api/v1/library/scan/stream"),
        ("GET", "/api/v1/library/scan/unmatched"),
        ("GET", "/api/v1/library/tracks/{file_id}/tags"),
        ("POST", "/api/v1/library/albums/{mbid}/reidentify"),
        ("POST", "/api/v1/library/albums/{mbid}/rescan"),
        ("POST", "/api/v1/library/scan/unmatched/resolve-batch"),
        ("POST", "/api/v1/library/scan/unmatched/{review_id}/resolve"),
        ("POST", "/api/v1/library/sync"),
        ("POST", "/api/v1/library/tracks/{file_id}"),
    }
)

# The target runtime filters out legacy-library settings: during an upgrade the
# library roots are owned by the migration, not editable over the API.
_LEGACY_LIBRARY_SETTINGS = frozenset(
    {
        ("DELETE", "/api/v1/settings/library/paths"),
        ("GET", "/api/v1/settings/library/path-mapping"),
        ("GET", "/api/v1/settings/library/roots"),
        ("POST", "/api/v1/settings/library/paths"),
        ("PUT", "/api/v1/settings/library/roots"),
    }
)

INTENTIONAL_OMISSIONS: frozenset[tuple[str, str]] = (
    _LIBRARY_TARGET_VARIANTS | _LEGACY_LIBRARY_SETTINGS
)


def _v1_operations(app: FastAPI) -> set[tuple[str, str]]:
    return {
        (method, route.path)
        for route in app.routes
        if getattr(route, "path", "").startswith("/api/v1")
        for method in getattr(route, "methods", set())
    }


def test_target_app_exposes_every_v1_route_main_does():
    from main import app as main_app
    from target_application import create_production_target_application

    missing = _v1_operations(main_app) - _v1_operations(
        create_production_target_application()
    )

    assert missing == INTENTIONAL_OMISSIONS, (
        "target application route omissions changed; update the exact allowlist "
        f"only for intentional runtime differences: {sorted(missing)}"
    )


def test_target_app_serves_the_torrent_routes():
    """Narrow guard on the specific regression, in case the broad check above
    ever grows an allowlist entry that would hide it."""
    from target_application import create_production_target_application

    operations = _v1_operations(create_production_target_application())
    paths = {path for _, path in operations}

    assert any("prowlarr" in p for p in paths), "prowlarr routes absent from target app"
    assert any("qbittorrent" in p for p in paths), (
        "qbittorrent routes absent from target app"
    )
