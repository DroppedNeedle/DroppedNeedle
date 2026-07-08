"""Mock Lidarr served via ``httpx.MockTransport`` - the executable record of the live
behaviour verified against Lidarr 3.1.3.4968 (see
``repositories/lidarr_import/lidarr_import_API_NOTES.md``). Drop-in for the injected
``httpx.AsyncClient`` in ``LidarrImportRepository``.

Endpoints:
- ``GET /api/v1/system/status`` -> 200 ``{version}`` with a good key, 401 with a bad key.
- ``GET /api/v1/artist`` -> 200 a plain JSON array covering every import case.

Auth is the ``X-Api-Key`` header (verified live). ``GOOD_KEY`` authenticates.
"""

import httpx

GOOD_KEY = "test-key"
VERSION = "3.1.3.4968"

# Valid MBIDs (UUID form) so is_valid_mbid passes for the ones we expect to import.
MBID_AUTO = "11111111-1111-1111-1111-111111111111"  # monitored + monitorNewItems=all
MBID_PLAIN = "22222222-2222-2222-2222-222222222222"  # monitored + none
MBID_UNMONITORED = "33333333-3333-3333-3333-333333333333"  # excluded
MBID_ENDED = "44444444-4444-4444-4444-444444444444"  # monitored, status ended (A3)
MBID_AUTO2 = "55555555-5555-5555-5555-555555555555"  # a second monitored=all

# The corpus. Extra fields (id, cleanName, statistics, ...) mirror the live shape and must
# be ignored by our tolerant decode.
ARTISTS = [
    {
        "id": 7,
        "artistName": "Auto Artist",
        "foreignArtistId": MBID_AUTO,
        "monitored": True,
        "monitorNewItems": "all",
        "status": "continuing",
        "statistics": {"trackFileCount": 42, "albumCount": 5},
    },
    {
        "id": 8,
        "artistName": "Plain Artist",
        "foreignArtistId": MBID_PLAIN,
        "monitored": True,
        "monitorNewItems": "none",
        "status": "continuing",
        "statistics": {"trackFileCount": 10},
    },
    {
        "id": 9,
        "artistName": "Unmonitored Artist",
        "foreignArtistId": MBID_UNMONITORED,
        "monitored": False,
        "monitorNewItems": "all",
        "status": "continuing",
    },
    {
        # Empty foreignArtistId -> skipped_invalid.
        "id": 10,
        "artistName": "No MBID Artist",
        "foreignArtistId": "",
        "monitored": True,
        "monitorNewItems": "none",
        "status": "continuing",
    },
    {
        # mbId present but foreignArtistId empty -> confirms we ignore mbId (still skipped).
        "id": 11,
        "artistName": "Legacy mbId Artist",
        "foreignArtistId": "",
        "mbId": "99999999-9999-9999-9999-999999999999",
        "monitored": True,
        "monitorNewItems": "none",
        "status": "continuing",
    },
    {
        # status=ended is still imported (A3).
        "id": 12,
        "artistName": "Ended Artist",
        "foreignArtistId": MBID_ENDED,
        "monitored": True,
        "monitorNewItems": "none",
        "status": "ended",
    },
    {
        "id": 13,
        "artistName": "Second Auto Artist",
        "foreignArtistId": MBID_AUTO2,
        "monitored": True,
        "monitorNewItems": "all",
        "status": "continuing",
    },
]

# The MBIDs a correct import should follow (monitored + valid MBID), and the auto-download
# subset (monitored + monitorNewItems=all).
EXPECTED_FOLLOW_MBIDS = {MBID_AUTO, MBID_PLAIN, MBID_ENDED, MBID_AUTO2}
EXPECTED_AUTO_DOWNLOAD_MBIDS = {MBID_AUTO, MBID_AUTO2}


def lidarr_handler(request: httpx.Request) -> httpx.Response:
    if request.headers.get("X-Api-Key") != GOOD_KEY:
        return httpx.Response(401, json={"error": "Unauthorized"})
    path = request.url.path
    if path.endswith("/api/v1/system/status"):
        return httpx.Response(200, json={"version": VERSION, "appName": "Lidarr"})
    if path.endswith("/api/v1/artist"):
        return httpx.Response(200, json=ARTISTS)
    return httpx.Response(404, json={"error": "not found"})


def unreachable_handler(request: httpx.Request) -> httpx.Response:
    raise httpx.ConnectError("connection refused")


def garbage_handler(request: httpx.Request) -> httpx.Response:
    if request.headers.get("X-Api-Key") != GOOD_KEY:
        return httpx.Response(401, json={"error": "Unauthorized"})
    # 200 but a non-array/HTML body -> decode failure -> LidarrImportError.
    return httpx.Response(200, content=b"<html>not json</html>")


def client_for(handler) -> httpx.AsyncClient:
    """An ``httpx.AsyncClient`` whose transport serves ``handler`` - drop-in for the injected
    client in ``LidarrImportRepository``."""
    return httpx.AsyncClient(transport=httpx.MockTransport(handler))
