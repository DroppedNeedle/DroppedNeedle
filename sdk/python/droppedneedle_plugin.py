"""Helper for writing DroppedNeedle plugins in Python.

A plugin is a class. DroppedNeedle starts it as its own process with

    python3 -m droppedneedle_plugin <module>:<ClassName>

(the ``entrypoint`` in ``plugin.toml``), builds one instance with a
``PluginContext``, and calls its methods over JSON-RPC on stdin and stdout.
This module does all of the protocol work, so a plugin only writes the
methods for its capabilities, with the same names and shapes as in
DroppedNeedle v2:

    from droppedneedle_plugin import PluginPurchaseLink

    class MyPlugin:
        def __init__(self, context):
            self.ctx = context

        async def purchase_links(self, artist, album, release_group_mbid):
            return [PluginPurchaseLink(label="Shop", url="https://shop.test/x")]

Rules of the road:

- Do not print to stdout. stdout carries the protocol; this module points
  ``print`` and fd 1 at stderr for you, and stderr lines land in the
  server log under your plugin's name. Use ``self.ctx.logger``.
- Methods may be ``async`` or plain functions. Blocking work in an
  ``async`` method stalls your plugin (not the server); move it to
  ``asyncio.to_thread``.
- Only the standard library is used here. Python 3.9 or newer.

To run a class from your own script instead, set ``command`` in
``plugin.toml`` and call ``droppedneedle_plugin.run(MyPlugin)``.
"""

from __future__ import annotations

import asyncio
import contextvars
import dataclasses
import importlib
import json
import logging
import os
import re
import sys
import threading
import traceback
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Optional

__all__ = [
    "PROTOCOL_VERSION",
    "DownloadFileRef",
    "DownloadMaterialization",
    "DownloadTaskStatus",
    "EnqueueRequest",
    "IndexerResult",
    "MountDiagnosis",
    "PluginAlbumEnrichment",
    "PluginArtistEnrichment",
    "PluginContext",
    "PluginEvent",
    "PluginPurchaseLink",
    "PluginRouteResponse",
    "PluginSearchResult",
    "PluginStreamRef",
    "Record",
    "ScrobbleEvent",
    "ServiceStatus",
    "TaskHandle",
    "run",
]

PROTOCOL_VERSION = 1

# --------------------------------------------------------------------------
# Types. Field names match v2's plugin protocol types.
# --------------------------------------------------------------------------


class Record(dict):
    """A dict you can also read with attributes: ``event.payload.task_id``."""

    def __getattr__(self, name: str) -> Any:
        try:
            return self[name]
        except KeyError as exc:
            raise AttributeError(name) from exc


@dataclass
class ServiceStatus:
    status: str = "ok"
    message: str = ""
    version: Optional[str] = None


@dataclass
class ScrobbleEvent:
    artist: str = ""
    track: str = ""
    album: Optional[str] = None
    timestamp: int = 0
    duration_ms: Optional[int] = None
    recording_mbid: Optional[str] = None


@dataclass
class PluginPurchaseLink:
    label: str
    url: str
    kind: str = "digital"


@dataclass
class PluginEvent:
    kind: str
    payload: Record
    causation_id: str = ""


@dataclass
class PluginRouteResponse:
    status: int = 200
    body: Any = None


@dataclass
class DownloadFileRef:
    username: str = ""
    filename: str = ""
    size: int = 0


@dataclass
class EnqueueRequest:
    task_id: str
    source: str
    files: list = field(default_factory=list)
    payload: str = ""
    job_name: Optional[str] = None
    download_type: str = "album"


@dataclass
class TaskHandle:
    source: str = ""
    username: str = ""
    filenames: list = field(default_factory=list)
    job_name: str = ""
    nzo_id: str = ""
    plugin_token: str = ""


@dataclass
class DownloadTaskStatus:
    task_id: str = ""
    status: str = ""
    files_total: int = 0
    files_completed: int = 0
    files_failed: int = 0
    bytes_total: int = 0
    bytes_downloaded: int = 0
    progress_percent: float = 0.0
    error: Optional[str] = None
    succeeded_filenames: list = field(default_factory=list)
    has_active_transfer: bool = False
    matched_transfers: int = 0
    queue_position_start: Optional[int] = None
    queue_position_end: Optional[int] = None


@dataclass
class DownloadMaterialization:
    state: str = "missing"
    nzo_id: str = ""
    remote_storage: str = ""
    mount_root: str = ""
    workspace_path: str = ""
    file_paths: list = field(default_factory=list)
    mount_healthy: bool = True


@dataclass
class MountDiagnosis:
    supported: bool = False
    completed_downloads: int = 0
    mount_has_files: bool = True
    resolvable_downloads: int = 0
    sampled_downloads: int = 0
    client_downloads_dir: Optional[str] = None


@dataclass
class PluginSearchResult:
    title: str = ""
    size_bytes: int = 0
    score: float = 0.0
    quality_tier: str = ""
    files: list = field(default_factory=list)
    payload: str = ""
    nzb_url: str = ""
    usenet_date: Optional[float] = None


@dataclass
class IndexerResult:
    source: str = ""
    plugin: Optional[PluginSearchResult] = None


@dataclass
class PluginArtistEnrichment:
    biography: Optional[str] = None
    links: list = field(default_factory=list)
    tags: list = field(default_factory=list)
    image_urls: list = field(default_factory=list)


@dataclass
class PluginAlbumEnrichment:
    biography: Optional[str] = None
    links: list = field(default_factory=list)
    tags: list = field(default_factory=list)
    image_urls: list = field(default_factory=list)


@dataclass
class PluginStreamRef:
    path: str = ""
    url: str = ""
    content_type: str = ""
    duration_seconds: Optional[float] = None


def _build(cls: type, data: Any) -> Any:
    """Build a dataclass from a dict, ignoring keys it does not know."""
    if not isinstance(data, dict):
        data = {}
    names = {item.name for item in dataclasses.fields(cls)}
    return cls(**{key: value for key, value in data.items() if key in names})


def _to_json(value: Any) -> Any:
    """Turn plugin return values into plain JSON data."""
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return {item.name: _to_json(getattr(value, item.name)) for item in dataclasses.fields(value)}
    if isinstance(value, dict):
        return {str(key): _to_json(item) for key, item in value.items()}
    if isinstance(value, (list, tuple, set, frozenset)):
        return [_to_json(item) for item in value]
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, bytes):
        return value.decode("utf-8", "replace")
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if hasattr(value, "__dict__"):
        return {key: _to_json(item) for key, item in vars(value).items() if not key.startswith("_")}
    return str(value)


# --------------------------------------------------------------------------
# Context
# --------------------------------------------------------------------------


class Response:
    """An HTTP response from ``ctx.http``."""

    def __init__(self, status_code: int, headers: dict, content: bytes, url: str) -> None:
        self.status_code = status_code
        self.headers = headers
        self.content = content
        self.url = url

    @property
    def text(self) -> str:
        return self.content.decode("utf-8", "replace")

    @property
    def is_success(self) -> bool:
        return 200 <= self.status_code < 300

    def json(self) -> Any:
        return json.loads(self.content or b"null")

    def raise_for_status(self) -> "Response":
        if self.status_code >= 400:
            raise RuntimeError(f"HTTP {self.status_code} from {self.url}")
        return self


class HttpClient:
    """A small async HTTP client over the standard library.

    Mirrors the parts of ``httpx.AsyncClient`` v2 plugins used:
    ``get``/``post``/``put``/``delete``/``head`` with ``params``,
    ``headers``, ``json``, ``data``, ``content`` and ``timeout``. Requests
    run in a worker thread, so they never block other calls.
    """

    def __init__(self, user_agent: str) -> None:
        self._user_agent = user_agent

    async def request(
        self,
        method: str,
        url: str,
        *,
        params: Optional[dict] = None,
        headers: Optional[dict] = None,
        json: Any = None,  # noqa: A002 - httpx-compatible name
        data: Any = None,
        content: Optional[bytes] = None,
        timeout: float = 30.0,
    ) -> Response:
        return await asyncio.to_thread(
            self._send, method, url, params, headers, json, data, content, timeout
        )

    def _send(self, method, url, params, headers, json_body, data, content, timeout) -> Response:
        if params:
            joiner = "&" if urllib.parse.urlsplit(url).query else "?"
            url = url + joiner + urllib.parse.urlencode(params, doseq=True)
        sent_headers = {"User-Agent": self._user_agent}
        sent_headers.update(headers or {})
        body = content
        if json_body is not None:
            body = json.dumps(_to_json(json_body)).encode("utf-8")
            sent_headers.setdefault("Content-Type", "application/json")
        elif isinstance(data, dict):
            body = urllib.parse.urlencode(data, doseq=True).encode("utf-8")
            sent_headers.setdefault("Content-Type", "application/x-www-form-urlencoded")
        elif isinstance(data, (bytes, str)):
            body = data.encode("utf-8") if isinstance(data, str) else data
        request = urllib.request.Request(url, data=body, headers=sent_headers, method=method.upper())
        try:
            with urllib.request.urlopen(request, timeout=timeout) as answer:  # noqa: S310 - plugin's own URL
                return Response(answer.status, dict(answer.headers), answer.read(), answer.geturl())
        except urllib.error.HTTPError as error:
            return Response(error.code, dict(error.headers or {}), error.read() or b"", url)

    async def get(self, url: str, **kwargs: Any) -> Response:
        return await self.request("GET", url, **kwargs)

    async def head(self, url: str, **kwargs: Any) -> Response:
        return await self.request("HEAD", url, **kwargs)

    async def post(self, url: str, **kwargs: Any) -> Response:
        return await self.request("POST", url, **kwargs)

    async def put(self, url: str, **kwargs: Any) -> Response:
        return await self.request("PUT", url, **kwargs)

    async def delete(self, url: str, **kwargs: Any) -> Response:
        return await self.request("DELETE", url, **kwargs)


_WORD = re.compile(r"[^\w]+", re.UNICODE)


def _tokens(text: str) -> set:
    return {word for word in _WORD.split((text or "").casefold()) if word}


class ScoringHelper:
    """Optional match scores (0 to 1) by word overlap. Using them is never
    required; the server applies its own policy to whatever score you
    report."""

    def album_match(self, artist: str, album: str, candidate_title: str) -> float:
        wanted = _tokens(artist) | _tokens(album)
        if not wanted:
            return 0.0
        return round(len(wanted & _tokens(candidate_title)) / len(wanted), 4)

    def track_match(self, artist: str, track: str, filename: str) -> float:
        return self.album_match(artist, track, filename)


_handling_causation: contextvars.ContextVar = contextvars.ContextVar("causation", default=None)


class PluginContext:
    """What a plugin gets from the host.

    - ``settings``: the admin's saved values, updated in place when they
      change (secrets arrive decrypted).
    - ``http``: an async HTTP client.
    - ``logger``: a logger whose lines land in the server log.
    - ``scoring``: optional match helpers for indexers.
    - ``name``, ``plugin_dir``, ``data_dir``: your name, your code folder
      and your writable folder (also the working directory).
    - ``await publish(kind, payload)``: publish a hint (``publisher``).
    - ``await state_get(key)`` / ``await state_set(key, value)``: small
      string values the server keeps in its database for you.
    """

    def __init__(self, server: "_Server", init: dict) -> None:
        plugin = init.get("plugin") or {}
        self.name: str = plugin.get("name", "plugin")
        self.version: str = plugin.get("version", "")
        self.plugin_dir = Path(init.get("plugin_dir") or ".")
        self.data_dir = Path(init.get("data_dir") or ".")
        self.settings: dict = dict(init.get("settings") or {})
        self.logger = logging.getLogger(self.name)
        self.http = HttpClient(f"DroppedNeedle-Plugin/{self.name}")
        self.scoring = ScoringHelper()
        self._server = server

    async def publish(self, kind: str, payload: Optional[dict] = None) -> Record:
        params = {"kind": kind, "payload": _to_json(payload or {})}
        causation = _handling_causation.get()
        if causation:
            params["causation_id"] = causation
        return Record(await self._server.request("host.publish", params))

    async def state_get(self, key: str) -> Optional[str]:
        answer = await self._server.request("host.state.get", {"key": key})
        return (answer or {}).get("value")

    async def state_set(self, key: str, value: str) -> None:
        await self._server.request("host.state.set", {"key": key, "value": str(value)})


# --------------------------------------------------------------------------
# Protocol
# --------------------------------------------------------------------------

METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
PLUGIN_ERROR = -32000

_CAPABILITY_METHODS = {
    "scrobbler": ("on_scrobble",),
    "purchase_links": ("purchase_links",),
    "subscriber": ("on_event",),
    "publisher": (),
    "scheduler": ("on_tick",),
    "metadata_provider": ("enrich_artist", "enrich_album"),
    "streaming_source": ("resolve_stream",),
    "indexer": ("search_album", "search_track"),
    "download_client": ("enqueue",),
}


class RpcFailure(Exception):
    def __init__(self, code: int, message: str) -> None:
        super().__init__(message)
        self.code = code


async def _call(function: Any, *args: Any, **kwargs: Any) -> Any:
    result = function(*args, **kwargs)
    if asyncio.iscoroutine(result) or isinstance(result, asyncio.Future):
        result = await result
    return result


def _search_results(found: Any) -> list:
    out = []
    for item in found or []:
        if isinstance(item, IndexerResult):
            if item.plugin is not None:
                out.append(_to_json(item.plugin))
        elif isinstance(item, dict) and isinstance(item.get("plugin"), dict):
            out.append(item["plugin"])
        else:
            out.append(_to_json(item))
    return out


def _handle(params: dict) -> TaskHandle:
    return _build(TaskHandle, (params or {}).get("handle"))


class _Server:
    def __init__(self, factory: Any) -> None:
        self._factory = factory
        self._instance: Any = None
        self._context: Optional[PluginContext] = None
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._write_lock = threading.Lock()
        self._next_id = 1
        self._waiting: dict = {}
        self._tasks: dict = {}
        self._done: Optional[asyncio.Future] = None
        # Keep the protocol stream for ourselves and send anything else
        # written to fd 1 (print, child processes) to stderr.
        self._out = os.fdopen(os.dup(1), "wb", buffering=0)
        os.dup2(2, 1)
        sys.stdout = sys.stderr

    # -- wire --

    def _send(self, message: dict) -> None:
        message["jsonrpc"] = "2.0"
        line = json.dumps(message, ensure_ascii=False, separators=(",", ":")) + "\n"
        with self._write_lock:
            self._out.write(line.encode("utf-8"))

    def _reply(self, request_id: Any, result: Any = None, error: Optional[RpcFailure] = None) -> None:
        if error is not None:
            self._send({"id": request_id, "error": {"code": error.code, "message": str(error)}})
        else:
            self._send({"id": request_id, "result": _to_json(result)})

    async def request(self, method: str, params: dict) -> Any:
        assert self._loop is not None
        request_id = self._next_id
        self._next_id += 1
        waiter = self._loop.create_future()
        self._waiting[request_id] = waiter
        self._send({"id": request_id, "method": method, "params": params})
        try:
            return await asyncio.wait_for(waiter, timeout=15)
        finally:
            self._waiting.pop(request_id, None)

    def _read_stdin(self) -> None:
        stream = sys.stdin.buffer
        while True:
            line = stream.readline()
            if not line:
                break
            assert self._loop is not None
            self._loop.call_soon_threadsafe(self._on_line, line)
        assert self._loop is not None
        self._loop.call_soon_threadsafe(self._finish)

    def _finish(self) -> None:
        if self._done is not None and not self._done.done():
            self._done.set_result(None)

    def _on_line(self, line: bytes) -> None:
        try:
            message = json.loads(line)
        except ValueError:
            logging.getLogger("droppedneedle_plugin").warning("host sent a line that is not JSON")
            return
        if not isinstance(message, dict):
            return
        method = message.get("method")
        request_id = message.get("id")
        if method is None:
            waiter = self._waiting.get(request_id)
            if waiter is not None and not waiter.done():
                if "error" in message:
                    error = message.get("error") or {}
                    waiter.set_exception(RuntimeError(error.get("message", "host request failed")))
                else:
                    waiter.set_result(message.get("result"))
            return
        params = message.get("params") or {}
        if request_id is None:
            self._notification(method, params)
            return
        task = asyncio.ensure_future(self._dispatch(request_id, method, params))
        self._tasks[request_id] = task
        task.add_done_callback(lambda _done, key=request_id: self._tasks.pop(key, None))

    def _notification(self, method: str, params: dict) -> None:
        if method == "settings.update" and self._context is not None:
            self._context.settings.clear()
            self._context.settings.update(params.get("settings") or {})
        elif method == "$/cancel":
            task = self._tasks.get(params.get("id"))
            if task is not None:
                task.cancel()

    async def _dispatch(self, request_id: Any, method: str, params: Any) -> None:
        try:
            result = await self._run(method, params if isinstance(params, dict) else {})
        except asyncio.CancelledError:
            return
        except RpcFailure as failure:
            self._reply(request_id, error=failure)
            return
        except Exception as exc:  # noqa: BLE001 - report any plugin failure to the host
            logging.getLogger("droppedneedle_plugin").error(
                "%s failed: %s", method, "".join(traceback.format_exception(type(exc), exc, exc.__traceback__)).strip()
            )
            self._reply(request_id, error=RpcFailure(PLUGIN_ERROR, f"{type(exc).__name__}: {exc}"))
            return
        self._reply(request_id, result)
        if method == "shutdown":
            self._finish()

    def _method(self, name: str) -> Any:
        found = getattr(self._instance, name, None)
        if found is None:
            raise RpcFailure(METHOD_NOT_FOUND, f"plugin has no {name}()")
        return found

    # -- methods --

    async def _run(self, method: str, params: dict) -> Any:
        if method == "initialize":
            return self._initialize(params)
        if method == "shutdown":
            return {}
        if self._instance is None:
            raise RpcFailure(INVALID_PARAMS, "initialize first")
        inst = self._instance
        if method == "plugin.health":
            configured = True
            if hasattr(inst, "is_configured"):
                configured = bool(await _call(inst.is_configured))
            status = ServiceStatus()
            if hasattr(inst, "health_check"):
                status = await _call(inst.health_check)
            data = _to_json(status) if status is not None else {}
            return {"status": data.get("status", "ok"), "message": data.get("message", ""), "configured": configured}
        if method == "scrobbler.on_scrobble":
            await _call(self._method("on_scrobble"), _build(ScrobbleEvent, params))
            return None
        if method == "purchase_links.get":
            return await _call(
                self._method("purchase_links"),
                params.get("artist", ""),
                params.get("album", ""),
                params.get("release_group_mbid", ""),
            ) or []
        if method == "subscriber.on_event":
            event = PluginEvent(
                kind=params.get("kind", ""),
                payload=Record(params.get("payload") or {}),
                causation_id=params.get("causation_id", ""),
            )
            token = _handling_causation.set(event.causation_id or None)
            try:
                await _call(self._method("on_event"), event)
            finally:
                _handling_causation.reset(token)
            return None
        if method == "scheduler.on_tick":
            await _call(self._method("on_tick"))
            return None
        if method == "routes.handle":
            answer = await _call(
                self._method("handle_route"),
                params.get("method", "GET"),
                params.get("subpath", ""),
                dict(params.get("query") or {}),
                params.get("body"),
            )
            if isinstance(answer, PluginRouteResponse):
                return answer
            if isinstance(answer, dict) and "status" in answer:
                return {"status": answer["status"], "body": answer.get("body")}
            return PluginRouteResponse(status=200, body=answer)
        if method == "metadata.enrich_artist":
            return await _call(
                self._method("enrich_artist"),
                artist_name=params.get("artist_name", ""),
                mbid=params.get("mbid"),
                timeout=15.0,
            )
        if method == "metadata.enrich_album":
            return await _call(
                self._method("enrich_album"),
                artist_name=params.get("artist_name", ""),
                album_title=params.get("album_title", ""),
                mbid=params.get("mbid"),
                timeout=15.0,
            )
        if method == "stream.resolve":
            return await _call(
                self._method("resolve_stream"), params.get("recording_mbid", ""), params.get("user_id", "")
            )
        if method == "indexer.search_album":
            found = await _call(
                self._method("search_album"),
                params.get("artist_name", ""),
                params.get("album_title", ""),
                params.get("year"),
                params.get("track_count"),
                timeout=float(params.get("timeout") or 30.0),
            )
            return _search_results(found)
        if method == "indexer.search_track":
            found = await _call(
                self._method("search_track"),
                params.get("artist_name", ""),
                params.get("track_title", ""),
                params.get("album_title"),
                params.get("duration_seconds"),
                timeout=float(params.get("timeout") or 30.0),
            )
            return _search_results(found)
        if method == "download_client.enqueue":
            request = _build(EnqueueRequest, params)
            request.files = [_build(DownloadFileRef, item) for item in params.get("files") or []]
            handle = await _call(self._method("enqueue"), request)
            return handle
        if method == "download_client.status":
            return await _call(self._method("get_status"), _handle(params))
        if method == "download_client.inspect":
            return await _call(self._method("inspect_materialization"), _handle(params))
        if method == "download_client.discard":
            return bool(await _call(self._method("discard_client_artifacts"), _handle(params)))
        if method == "download_client.abort":
            return bool(await _call(self._method("abort"), _handle(params)))
        raise RpcFailure(METHOD_NOT_FOUND, f"unknown method {method}")

    def _initialize(self, params: dict) -> dict:
        wanted = params.get("protocol_version")
        if wanted != PROTOCOL_VERSION:
            raise RpcFailure(INVALID_PARAMS, f"this helper speaks protocol {PROTOCOL_VERSION}, not {wanted}")
        self._context = PluginContext(self, params)
        self._instance = self._factory(self._context)
        declared = (params.get("plugin") or {}).get("capabilities") or []
        implemented = []
        for capability in declared:
            names = _CAPABILITY_METHODS.get(capability)
            if names is None:
                continue
            if not names and capability == "publisher":
                implemented.append(capability)
            elif any(hasattr(self._instance, name) for name in names):
                implemented.append(capability)
        return {"protocol_version": PROTOCOL_VERSION, "capabilities": implemented}

    # -- lifecycle --

    async def serve(self) -> None:
        self._loop = asyncio.get_running_loop()
        self._done = self._loop.create_future()
        reader = threading.Thread(target=self._read_stdin, name="droppedneedle-stdin", daemon=True)
        reader.start()
        await self._done
        for task in list(self._tasks.values()):
            task.cancel()


def _configure_logging() -> None:
    handler = logging.StreamHandler(sys.stderr)
    handler.setFormatter(logging.Formatter("%(levelname)s %(message)s"))
    root = logging.getLogger()
    root.handlers[:] = [handler]
    root.setLevel(logging.INFO)


def run(factory: Any) -> None:
    """Serve one plugin class (or any callable taking the context) until
    the host closes stdin or asks it to shut down."""
    _configure_logging()
    server = _Server(factory)
    asyncio.run(server.serve())


def _load(entrypoint: str) -> Any:
    module_name, _, class_name = entrypoint.partition(":")
    if not module_name or not class_name:
        raise SystemExit(f"entrypoint must be '<module>:<ClassName>', got {entrypoint!r}")
    module = importlib.import_module(module_name)
    try:
        return getattr(module, class_name)
    except AttributeError:
        raise SystemExit(f"{module_name} has no {class_name}") from None


def main(argv: list) -> None:
    if len(argv) != 2:
        raise SystemExit("usage: python3 -m droppedneedle_plugin <module>:<ClassName>")
    run(_load(argv[1]))


if __name__ == "__main__":
    # `python3 -m` runs this file as `__main__`; plugins import it as
    # `droppedneedle_plugin`. Run through the imported copy so both see
    # the same classes.
    import droppedneedle_plugin

    droppedneedle_plugin.main(sys.argv)
