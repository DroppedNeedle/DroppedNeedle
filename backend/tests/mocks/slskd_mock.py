"""FastAPI app mimicking slskd 0.25.1's /api/v0 endpoints with canned state.

Used by Phase 6 unit tests (mounted via httpx ASGITransport), Phase 7 E2E
tests, and local dev (``uvicorn backend.tests.mocks.slskd_mock:app --port 5031``).
Shapes mirror the verified slskd 0.25.1 JSON (camelCase keys, comma-joined
``state`` flags, plain-array enqueue returning ``{Enqueued, Failed}``, no batch
GUID - correlation is by ``(username, filename)``).
"""

import uuid
from typing import Any

from fastapi import Depends, FastAPI, Header, HTTPException

app = FastAPI(title="slskd-mock")

# In-memory state: search_id -> responses, and username -> list[transfer].
_SEARCHES: dict[str, list[dict[str, Any]]] = {}
_TRANSFERS: dict[str, list[dict[str, Any]]] = {}

# Optional strict mode for auth tests (issue #193): when set, any non-empty key
# that is not an exact match 401s like a wrong key or a key-CIDR deny (slskd
# returns 401 for both). Unset (None) preserves the legacy behaviour where any
# non-empty key passes.
_EXPECTED_API_KEY: str | None = None


def set_expected_api_key(key: str | None) -> None:
    """Pin the key the mock accepts; None restores accept-any-non-empty."""
    global _EXPECTED_API_KEY
    _EXPECTED_API_KEY = key


def _require_api_key(x_api_key: str | None = Header(default=None)) -> None:
    if not x_api_key:
        raise HTTPException(status_code=401, detail="X-API-Key header required")
    if _EXPECTED_API_KEY is not None and x_api_key != _EXPECTED_API_KEY:
        raise HTTPException(status_code=401, detail="Invalid API key")


def _file(filename: str, size: int, *, ext: str, bitrate: int | None,
          bit_depth: int | None = None, sample_rate: int | None = None,
          length: float = 240.0) -> dict[str, Any]:
    entry: dict[str, Any] = {
        "filename": filename,
        "size": size,
        "extension": ext,
        "length": length,
        "code": 1,
        "isLocked": False,
    }
    if bitrate is not None:
        entry["bitRate"] = bitrate
    if bit_depth is not None:
        entry["bitDepth"] = bit_depth
    if sample_rate is not None:
        entry["sampleRate"] = sample_rate
    return entry


def _canned_responses() -> list[dict[str, Any]]:
    """Three peers: a complete FLAC album, a partial MP3 folder, and a junk folder."""
    alice_files = [
        _file(
            f"@@music\\Radiohead - OK Computer (1997)\\{n:02d} Track {n}.flac",
            30_000_000,
            ext="",  # slskd omits extension for some files (C6a) - parse from filename
            bitrate=None,  # lossless: bitRate absent (C6b)
            bit_depth=16,
            sample_rate=44100,
        )
        for n in range(1, 13)
    ]
    bob_files = [
        _file(
            f"/home/bob/Random Rips/{n:02d} song.mp3",
            8_000_000,
            ext="mp3",
            bitrate=320,
        )
        for n in range(1, 6)
    ]
    charlie_files = [
        _file(
            "/downloads/Various Artists - Unknown Album/track.mp3",
            7_000_000,
            ext="mp3",
            bitrate=128,
        )
    ]
    return [
        {
            "username": "alice",
            "hasFreeUploadSlot": True,
            "uploadSpeed": 2_000_000,
            "queueLength": 0,
            "fileCount": len(alice_files),
            "lockedFileCount": 0,
            "files": alice_files,
            "lockedFiles": [],
            "token": 1,
        },
        {
            "username": "bob",
            "hasFreeUploadSlot": False,
            "uploadSpeed": 500_000,
            "queueLength": 3,
            "fileCount": len(bob_files),
            "lockedFileCount": 0,
            "files": bob_files,
            "lockedFiles": [],
            "token": 2,
        },
        {
            "username": "charlie",
            "hasFreeUploadSlot": True,
            "uploadSpeed": 100_000,
            "queueLength": 0,
            "fileCount": len(charlie_files),
            "lockedFileCount": 0,
            "files": charlie_files,
            "lockedFiles": [],
            "token": 3,
        },
    ]


@app.get("/api/v0/application", dependencies=[Depends(_require_api_key)])
async def application() -> dict[str, Any]:
    return {
        "version": {"current": "0.25.1.0", "latest": "0.25.1.0", "isUpdateAvailable": False},
        "server": {"state": "Connected, LoggedIn", "address": "vps.slsknet.org"},
        "shares": {"directories": 277},
    }


@app.post("/api/v0/searches", status_code=200, dependencies=[Depends(_require_api_key)])
async def start_search(body: dict[str, Any]) -> dict[str, Any]:
    search_id = uuid.uuid4().hex
    _SEARCHES[search_id] = _canned_responses()
    return {
        "id": search_id,
        "searchText": body.get("searchText", ""),
        "state": "Completed, Succeeded",
        "isComplete": True,
        "fileCount": sum(r["fileCount"] for r in _SEARCHES[search_id]),
        "responseCount": len(_SEARCHES[search_id]),
        "lockedFileCount": 0,
        "token": 12345,
    }


@app.get("/api/v0/searches/{search_id}", dependencies=[Depends(_require_api_key)])
async def search_state(search_id: str) -> dict[str, Any]:
    if search_id not in _SEARCHES:
        raise HTTPException(status_code=404, detail="search not found")
    return {
        "id": search_id,
        "state": "Completed, Succeeded",
        "isComplete": True,
        "responseCount": len(_SEARCHES[search_id]),
    }


@app.get("/api/v0/searches/{search_id}/responses", dependencies=[Depends(_require_api_key)])
async def search_responses(search_id: str) -> list[dict[str, Any]]:
    if search_id not in _SEARCHES:
        raise HTTPException(status_code=404, detail="search not found")
    return _SEARCHES[search_id]


@app.post(
    "/api/v0/transfers/downloads/{username}",
    status_code=201,
    dependencies=[Depends(_require_api_key)],
)
async def enqueue(username: str, files: list[dict[str, Any]]) -> dict[str, Any]:
    """Plain array body ``[{filename, size}]`` -> 201 ``{Enqueued, Failed}``."""
    bucket = _TRANSFERS.setdefault(username, [])
    enqueued = []
    for entry in files:
        filename = entry["filename"]
        size = int(entry.get("size", 0))
        bucket.append(
            {
                "id": uuid.uuid4().hex,
                "username": username,
                "filename": filename,
                "size": size,
                "bytesTransferred": size,
                "bytesRemaining": 0,
                "percentComplete": 100.0,
                "averageSpeed": 1_000_000.0,
                "state": "Completed, Succeeded",
                "direction": "Download",
            }
        )
        enqueued.append({"filename": filename, "size": size})
    return {"Enqueued": enqueued, "Failed": []}


@app.get("/api/v0/transfers/downloads/{username}", dependencies=[Depends(_require_api_key)])
async def user_transfers(username: str) -> dict[str, Any]:
    """Per-user transfers grouped as directories -> files (slskd shape)."""
    files = _TRANSFERS.get(username, [])
    directories: dict[str, list[dict[str, Any]]] = {}
    for transfer in files:
        parent = transfer["filename"].replace("\\", "/").rsplit("/", 1)[0]
        directories.setdefault(parent, []).append(transfer)
    return {
        "username": username,
        "directories": [
            {"directory": directory, "fileCount": len(items), "files": items}
            for directory, items in directories.items()
        ],
    }


@app.delete(
    "/api/v0/transfers/downloads/{username}/{transfer_id}",
    dependencies=[Depends(_require_api_key)],
)
async def remove_transfer(username: str, transfer_id: str, remove: bool = False) -> dict[str, Any]:
    bucket = _TRANSFERS.get(username, [])
    _TRANSFERS[username] = [t for t in bucket if t["id"] != transfer_id]
    return {"removed": transfer_id}


def reset_state() -> None:
    """Clear in-memory state between tests."""
    _SEARCHES.clear()
    _TRANSFERS.clear()
    set_expected_api_key(None)
