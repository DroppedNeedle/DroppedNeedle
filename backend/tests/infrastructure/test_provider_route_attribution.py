"""BrainzMashEfficiency T10: per-route MB attribution telemetry.

Covers provider_counters.route_scope + the (provider, route, category) side
map, the main-key isolation guarantee, and the compat dispatcher stamps
(Subsonic _dispatch nested override + unchanged Subsonic error envelope;
Jellyfin _handle handler-name stamp incl. the "<lambda>" collapse). Plus the
/api MsgSpecRoute template stamp and the no-route-in-scope unknown stamp.
"""

import json
from contextlib import AsyncExitStack
from types import SimpleNamespace

import pytest
from fastapi import APIRouter, FastAPI
from fastapi.testclient import TestClient
from starlette.requests import Request
from starlette.responses import Response

from api.compat.common.ratelimit import compat_rate_limits
from api.compat.jellyfin import router as jellyfin_router
from api.compat.subsonic import router as subsonic_router
from core.exceptions import SubsonicError
from infrastructure.cache.cache_metrics import WindowedCounterMap
from infrastructure.msgspec_fastapi import MsgSpecRoute
from infrastructure.observability import provider_counters


@pytest.fixture
def fresh_counters(monkeypatch):
    monkeypatch.setattr(
        provider_counters, "_counters", provider_counters.ProviderCounterMap()
    )
    monkeypatch.setattr(provider_counters, "_route_counters", WindowedCounterMap())
    provider_counters.provider_route.set(provider_counters.UNKNOWN_ROUTE)
    compat_rate_limits.reset()
    yield provider_counters
    compat_rate_limits.reset()


def _route_totals(rows):
    return {
        (row["provider"], row["route"], row["request_category"]): row["count_total"]
        for row in rows
    }


def test_route_scopes_isolate_side_map_rows(fresh_counters):
    with provider_counters.route_scope("route-a"):
        provider_counters.record_provider_call(
            "musicbrainz", None, 200, category="lookup"
        )
        provider_counters.record_provider_call(
            "musicbrainz", None, 200, category="lookup"
        )
    with provider_counters.route_scope("route-b"):
        provider_counters.record_provider_call(
            "musicbrainz", None, 404, category="lookup"
        )

    rows = provider_counters.snapshot_provider_route_rows()
    assert _route_totals(rows) == {
        ("musicbrainz", "route-a", "lookup"): 2,
        ("musicbrainz", "route-b", "lookup"): 1,
    }
    by_key = {
        (row["provider"], row["route"], row["request_category"]): row for row in rows
    }
    assert by_key[("musicbrainz", "route-a", "lookup")][
        "rate_per_min_window"
    ] == round(2 / 60, 2)


def test_route_scopes_leave_main_key_untouched(fresh_counters):
    with provider_counters.route_scope("/api/v1/artists/{artist_id}"):
        provider_counters.record_provider_call(
            "musicbrainz", None, 200, category="lookup"
        )
    with provider_counters.route_scope("subsonic:ping"):
        provider_counters.record_provider_call(
            "musicbrainz", None, 200, category="lookup"
        )

    main_rows = provider_counters.snapshot_provider_rows()
    assert len(main_rows) == 1
    assert main_rows[0]["count_total"] == 2

    route_rows = provider_counters.snapshot_provider_route_rows()
    assert _route_totals(route_rows) == {
        ("musicbrainz", "/api/v1/artists/{artist_id}", "lookup"): 1,
        ("musicbrainz", "subsonic:ping", "lookup"): 1,
    }


def test_unstamped_calls_record_unknown_route(fresh_counters):
    provider_counters.record_provider_call(
        "musicbrainz", None, 200, category="lookup"
    )

    rows = provider_counters.snapshot_provider_route_rows()
    assert [(row["route"], row["count_total"]) for row in rows] == [("unknown", 1)]


def test_route_scope_nesting_restores_outer(fresh_counters):
    with provider_counters.route_scope("outer"):
        with provider_counters.route_scope("inner"):
            provider_counters.record_provider_call("musicbrainz", None, 200)
        provider_counters.record_provider_call("musicbrainz", None, 200)
    provider_counters.record_provider_call("musicbrainz", None, 200)

    rows = provider_counters.snapshot_provider_route_rows()
    assert _route_totals(rows) == {
        ("musicbrainz", "inner", "other"): 1,
        ("musicbrainz", "outer", "other"): 1,
        ("musicbrainz", "unknown", "other"): 1,
    }


def _http_request(path: str, query: bytes = b"f=json") -> Request:
    return Request(
        {
            "type": "http",
            "method": "GET",
            "path": path,
            "headers": [],
            "query_string": query,
            "client": ("127.0.0.1", 5000),
        }
    )


def _subsonic_services() -> SimpleNamespace:
    settings = SimpleNamespace(
        subsonic_enabled=True,
        advertise_server_name="test",
        transcoding_enabled=False,
    )
    return SimpleNamespace(
        preferences=SimpleNamespace(
            get_connect_apps_settings=lambda: settings
        ),
        version=SimpleNamespace(
            get_current_version=lambda: SimpleNamespace(version="test")
        ),
        app_passwords=SimpleNamespace(),
    )


@pytest.mark.asyncio
async def test_subsonic_dispatch_stamps_nested_route(
    fresh_counters, monkeypatch
):
    async def _recording_handler(ctx):
        provider_counters.record_provider_call(
            "musicbrainz", None, 200, category="lookup"
        )
        return Response("ok")

    monkeypatch.setitem(
        subsonic_router._HANDLERS, "getopensubsonicextensions", _recording_handler
    )
    request = _http_request("/subsonic/rest/getOpenSubsonicExtensions")

    response = await subsonic_router._dispatch(
        request, "getOpenSubsonicExtensions", _subsonic_services()
    )

    assert response.status_code == 200
    rows = provider_counters.snapshot_provider_route_rows()
    assert _route_totals(rows) == {
        ("musicbrainz", "subsonic:getopensubsonicextensions", "lookup"): 1
    }


@pytest.mark.asyncio
async def test_subsonic_dispatch_error_keeps_subsonic_envelope(
    fresh_counters, monkeypatch
):
    async def _failing_handler(ctx):
        raise SubsonicError(70, "Song not found")

    monkeypatch.setitem(
        subsonic_router._HANDLERS, "getopensubsonicextensions", _failing_handler
    )
    request = _http_request("/subsonic/rest/getOpenSubsonicExtensions")

    response = await subsonic_router._dispatch(
        request, "getOpenSubsonicExtensions", _subsonic_services()
    )

    body = json.loads(response.body)
    assert "subsonic-response" in body  # NOT the native {"error": {...}} shape
    assert "error" not in body
    sub = body["subsonic-response"]
    assert sub["status"] == "failed"
    assert sub["error"]["code"] == 70


def _jellyfin_services() -> SimpleNamespace:
    settings = SimpleNamespace(jellyfin_enabled=True)
    return SimpleNamespace(
        preferences=SimpleNamespace(
            get_connect_apps_settings=lambda: settings
        ),
        app_passwords=SimpleNamespace(),
    )


async def _jellyfin_recording_fn(request, services, user, **extra):
    provider_counters.record_provider_call(
        "musicbrainz", None, 200, category="browse"
    )
    return {"ok": True}


def _record_and_ok():
    provider_counters.record_provider_call(
        "musicbrainz", None, 200, category="browse"
    )
    return {"ok": True}


@pytest.mark.asyncio
async def test_jellyfin_handle_stamps_handler_name(fresh_counters):
    request = _http_request("/jellyfin/Users/Me")

    response = await jellyfin_router._handle(
        request, _jellyfin_services(), _jellyfin_recording_fn, auth=False
    )

    assert response.status_code == 200
    rows = provider_counters.snapshot_provider_route_rows()
    assert _route_totals(rows) == {
        ("musicbrainz", "_jellyfin_recording_fn", "browse"): 1
    }


@pytest.mark.asyncio
async def test_jellyfin_lambda_handlers_collapse_to_lambda(fresh_counters):
    request = _http_request("/jellyfin/Users/Me")
    collapse_fn = lambda r, s, u: _record_and_ok()  # noqa: E731 - pins the "<lambda>" collapse

    response = await jellyfin_router._handle(
        request, _jellyfin_services(), collapse_fn, auth=False
    )

    assert response.status_code == 200
    rows = provider_counters.snapshot_provider_route_rows()
    assert _route_totals(rows) == {("musicbrainz", "<lambda>", "browse"): 1}


def test_msgspec_route_stamps_template_not_raw_path(fresh_counters):
    router = APIRouter(route_class=MsgSpecRoute)

    @router.get("/api/v1/artists/{artist_id}")
    async def _endpoint(artist_id: str):
        provider_counters.record_provider_call(
            "musicbrainz", None, 200, category="lookup"
        )
        return {"ok": True}

    app = FastAPI()
    app.include_router(router)
    response = TestClient(app).get("/api/v1/artists/abc-123")

    assert response.status_code == 200
    rows = provider_counters.snapshot_provider_route_rows()
    assert _route_totals(rows) == {
        ("musicbrainz", "/api/v1/artists/{artist_id}", "lookup"): 1
    }


@pytest.mark.asyncio
async def test_msgspec_route_without_route_in_scope_stamps_unknown(fresh_counters):
    async def _endpoint():
        provider_counters.record_provider_call(
            "musicbrainz", None, 200, category="lookup"
        )
        return Response("ok")

    route = MsgSpecRoute(
        path="/api/v1/artists/{artist_id}", endpoint=_endpoint, methods=["GET"]
    )
    handler = route.get_route_handler()
    scope = {
        "type": "http",
        "method": "GET",
        "path": "/api/v1/artists/abc-123",
        "headers": [],
        "query_string": b"",
        "client": ("127.0.0.1", 5000),
        "app": FastAPI(),
        "fastapi_middleware_astack": AsyncExitStack(),
        "fastapi_inner_astack": AsyncExitStack(),
        "fastapi_function_astack": AsyncExitStack(),
        # No "route" key: simulates an unmatched request.
    }

    async def _receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    response = await handler(Request(scope, _receive))

    assert response.status_code == 200
    rows = provider_counters.snapshot_provider_route_rows()
    assert _route_totals(rows) == {("musicbrainz", "unknown", "lookup"): 1}
