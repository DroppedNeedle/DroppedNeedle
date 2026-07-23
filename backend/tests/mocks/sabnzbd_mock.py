"""Configurable SABnzbd mock (httpx.MockTransport), shaped from the owner's real
5.0.4 responses. Numbers are stringly-typed in the queue (mb/mbleft/percentage) and
real numbers in history (bytes), exactly like the live instance."""

import json

import httpx


class SabnzbdMock:
    def __init__(self) -> None:
        self.queue_slots: list[dict] = []
        self.history_slots: list[dict] = []
        self.categories = ["*", "movies", "tv", "audio", "software"]
        self.complete_dir = "/data/Downloads/complete"
        self.add_nzo_ids = ["nzo-test-1"]
        self.deleted: list[tuple[str, str]] = []  # (mode, value)
        self.delete_requests: list[dict] = []
        self.deleted_storage: list[str] = []
        self.retained_completed_storage: list[str] = []
        self.history_requests: list[dict] = []  # captured non-delete history call params

    # --- builders ---------------------------------------------------------------
    def queue_job(self, *, nzo_id, name, status, mb="100.0", mbleft="0.0", percentage="100"):
        self.queue_slots.append({
            "nzo_id": nzo_id, "filename": name, "status": status, "cat": "audio",
            "mb": mb, "mbleft": mbleft, "percentage": percentage, "timeleft": "0:00:00",
            "priority": "Normal",
        })
        return self

    def history_job(self, *, nzo_id, name, status, storage="", bytes_=0, fail_message=""):
        self.history_slots.append({
            "nzo_id": nzo_id, "name": name, "nzb_name": f"{name}.nzb", "status": status,
            "category": "audio", "storage": storage, "bytes": bytes_,
            "fail_message": fail_message, "password": None, "download_time": 100, "completed": 1,
        })
        return self

    # --- transport --------------------------------------------------------------
    def handler(self, request: httpx.Request) -> httpx.Response:
        p = request.url.params
        mode = p.get("mode")
        if mode == "version":
            return _json({"version": "5.0.4"})
        if mode == "get_cats":
            return _json({"categories": self.categories})
        if mode == "get_config":
            return _json({"config": {"misc": {"complete_dir": self.complete_dir},
                                     "categories": [{"name": c} for c in self.categories]}})
        if mode == "queue":
            if p.get("name") == "delete":
                self.deleted.append(("queue", p.get("value", "")))
                self.delete_requests.append(dict(p))
                return _json({"status": True})
            return _json({"queue": {"status": "Downloading" if self.queue_slots else "Idle",
                                    "paused": False, "slots": self.queue_slots}})
        if mode == "history":
            if p.get("name") == "delete":
                value = p.get("value", "")
                self.deleted.append(("history", value))
                self.delete_requests.append(dict(p))
                slot = next(
                    (item for item in self.history_slots if item["nzo_id"] == value),
                    None,
                )
                if slot is not None and p.get("del_files") == "1":
                    if slot["status"].lower() == "failed":
                        self.deleted_storage.append(slot["storage"])
                    else:
                        # SABnzbd 5.0.4 retains completed output even when history
                        # deletion receives del_files=1.
                        self.retained_completed_storage.append(slot["storage"])
                self.history_slots = [
                    item for item in self.history_slots if item["nzo_id"] != value
                ]
                return _json({"status": True})
            self.history_requests.append(dict(p))
            slots = self.history_slots
            # Honour the job filter the client now sends so a test can prove it's passed.
            if nzo := p.get("nzo_ids"):
                slots = [s for s in slots if s["nzo_id"] in nzo.split(",")]
            elif search := p.get("search"):
                slots = [s for s in slots if search in s["name"]]
            return _json({"history": {"slots": slots, "noofslots": len(slots)}})
        if mode == "addfile":
            return _json({"status": True, "nzo_ids": self.add_nzo_ids})
        return _json({"status": False, "error": f"unknown mode {mode}"})


def _json(body) -> httpx.Response:
    return httpx.Response(200, content=json.dumps(body).encode(), headers={"Content-Type": "application/json"})


def client_for(mock: SabnzbdMock) -> httpx.AsyncClient:
    return httpx.AsyncClient(transport=httpx.MockTransport(mock.handler))


def auth_error_handler(request: httpx.Request) -> httpx.Response:
    return _json({"status": False, "error": "API Key Incorrect"})
