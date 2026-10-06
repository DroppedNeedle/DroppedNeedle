"""Configurable NZBGet mock (httpx.MockTransport).

Shaped from the NZBGet JSON-RPC reference rather than a live instance: one POST
endpoint, positional params, a ``{"result": ...}`` or ``{"error": {...}}`` envelope,
64-bit sizes split into Lo/Hi halves, and config values left unexpanded
(``DestDir = ${MainDir}/dst``) exactly as ``config`` returns them.
"""

import json

import httpx

_LO_MASK = 0xFFFFFFFF


def _split(value: int) -> tuple[int, int]:
    """Split a 64-bit size the way NZBGet reports it."""
    return value & _LO_MASK, value >> 32


class NzbgetMock:
    def __init__(self) -> None:
        self.groups: list[dict] = []
        self.history_items: list[dict] = []
        self.version_string = "24.3"
        self.main_dir = "/downloads"
        self.dest_dir = "${MainDir}/dst"
        self.categories = ["music", "droppedneedle"]
        self.append_result = 7
        self.append_calls: list[list] = []
        self.editqueue_calls: list[list] = []
        self.editqueue_result = True

    # --- builders ---------------------------------------------------------------
    def group(
        self,
        *,
        nzb_id,
        name,
        status,
        file_size=100 * 1024 * 1024,
        remaining=0,
        dest_dir="",
        dupe_key="",
    ):
        size_lo, size_hi = _split(file_size)
        rem_lo, rem_hi = _split(remaining)
        self.groups.append({
            "NZBID": nzb_id,
            "NZBName": name,
            "NZBNicename": name,
            "Kind": "NZB",
            "Status": status,
            "Category": "music",
            "DestDir": dest_dir,
            "FinalDir": "",
            "DupeKey": dupe_key,
            "ActiveDownloads": 1 if status == "DOWNLOADING" else 0,
            "FileSizeLo": size_lo,
            "FileSizeHi": size_hi,
            "RemainingSizeLo": rem_lo,
            "RemainingSizeHi": rem_hi,
        })
        return self

    def history(
        self,
        *,
        nzb_id,
        name,
        status,
        dest_dir="",
        final_dir="",
        file_size=0,
        unpack_status="SUCCESS",
        par_status="SUCCESS",
        dupe_key="",
    ):
        size_lo, size_hi = _split(file_size)
        self.history_items.append({
            "NZBID": nzb_id,
            "Name": name,
            "NZBName": name,
            "Kind": "NZB",
            "Status": status,
            "Category": "music",
            "DestDir": dest_dir,
            "FinalDir": final_dir,
            "DupeKey": dupe_key,
            "ParStatus": par_status,
            "UnpackStatus": unpack_status,
            "MoveStatus": "SUCCESS",
            "ScriptStatus": "NONE",
            "DeleteStatus": "NONE",
            "FileSizeLo": size_lo,
            "FileSizeHi": size_hi,
            "HistoryTime": 1700000000,
        })
        return self

    def _config(self) -> list[dict]:
        options = [
            {"Name": "MainDir", "Value": self.main_dir},
            {"Name": "DestDir", "Value": self.dest_dir},
        ]
        for index, name in enumerate(self.categories, start=1):
            options.append({"Name": f"Category{index}.Name", "Value": name})
        return options

    # --- transport --------------------------------------------------------------
    def handler(self, request: httpx.Request) -> httpx.Response:
        payload = json.loads(request.content.decode() or "{}")
        method = payload.get("method")
        params = payload.get("params") or []
        if method == "version":
            return _json(self.version_string)
        if method == "config":
            return _json(self._config())
        if method == "listgroups":
            return _json(self.groups)
        if method == "history":
            return _json(self.history_items)
        if method == "append":
            self.append_calls.append(params)
            return _json(self.append_result)
        if method == "editqueue":
            self.editqueue_calls.append(params)
            return _json(self.editqueue_result)
        return _error(f"unknown method {method}")


def _json(result) -> httpx.Response:
    return httpx.Response(
        200,
        content=json.dumps({"version": "1.1", "result": result}).encode(),
        headers={"Content-Type": "application/json"},
    )


def _error(message: str) -> httpx.Response:
    return httpx.Response(
        200,
        content=json.dumps({"version": "1.1", "error": {"code": 2, "message": message}}).encode(),
        headers={"Content-Type": "application/json"},
    )


def client_for(mock: NzbgetMock) -> httpx.AsyncClient:
    return httpx.AsyncClient(transport=httpx.MockTransport(mock.handler))


def auth_error_handler(request: httpx.Request) -> httpx.Response:
    """NZBGet answers 401 for bad control credentials, before any JSON-RPC envelope."""
    return httpx.Response(401, content=b"Unauthorized")
