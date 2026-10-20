"""T6.2 - compat security: log redaction, CORS, rate limiting, audit log."""

import io
import json
import logging
from types import SimpleNamespace

import pytest
from fastapi import FastAPI
from fastapi.responses import Response
from fastapi.testclient import TestClient

from api.compat.common.cors import CompatCORSMiddleware
from api.compat.common.ratelimit import (
    CompatRateLimitState,
    reject_jellyfin,
    reject_subsonic,
    trusted_client_ip,
)
from api.compat.common.redact import (
    UvicornAccessCredentialFilter,
    install_uvicorn_access_credential_filter,
    redact_request_target,
    redacted_path,
)
from uvicorn.logging import AccessFormatter


# ----- redaction (pure) -----

def _req(query: str):
    return SimpleNamespace(url=SimpleNamespace(path="/subsonic/rest/ping", query=query))


def test_redacted_path_masks_secrets():
    out = redacted_path(_req("u=alice&p=sesame&t=abc&s=xy&apiKey=k&c=app"))
    assert "sesame" not in out and "abc" not in out and "k" not in out.split("apiKey=")[1][:3]
    assert "p=%2A%2A%2A" in out or "p=***" in out
    assert "u=alice" in out and "c=app" in out  # non-secrets preserved


def test_redacted_path_no_query():
    assert redacted_path(_req("")) == "/subsonic/rest/ping"


def test_redacted_path_masks_jellyfin_token_and_pw():
    out = redacted_path(_req("Pw=secret&token=tk&api_key=ak"))
    assert "secret" not in out and "tk" not in out and "ak" not in out


@pytest.mark.parametrize(
    "target",
    [
        "/subsonic/rest/ping?p=plain-secret&t=token-secret&s=salt-secret&apiKey=key-secret",
        "/subsonic/rest/ping?P=plain-secret&T=token-secret&S=salt-secret&APIKEY=key-secret",
        "/subsonic/rest/ping?%70=plain-secret&api%4Bey=key-secret&safe=visible",
        "/subsonic/rest/ping?p=first-secret&p=second-secret&token=&safe=visible",
        "/jellyfin/System/Info?Pw=jellyfin-password&api_key=jellyfin-key",
        "/subsonic/rest/getTranscodeStream?transcodeParams=stream-secret&safe=visible",
    ],
)
def test_request_target_redaction_covers_access_log_encodings(target):
    redacted = redact_request_target(target)
    assert "secret" not in redacted
    assert "jellyfin-password" not in redacted
    assert "jellyfin-key" not in redacted
    if "safe=" in target:
        assert "safe=visible" in redacted


def _format_real_uvicorn_access_record(target: str) -> str:
    stream = io.StringIO()
    handler = logging.StreamHandler(stream)
    handler.setFormatter(
        AccessFormatter('%(levelprefix)s %(client_addr)s - "%(request_line)s" %(status_code)s')
    )
    logger = logging.getLogger("uvicorn.access")
    previous_handlers = logger.handlers[:]
    previous_filters = logger.filters[:]
    previous_propagate = logger.propagate
    previous_level = logger.level
    try:
        logger.handlers = [handler]
        logger.filters = []
        logger.propagate = False
        logger.setLevel(logging.INFO)
        install_uvicorn_access_credential_filter()
        logger.info(
            '%s - "%s %s HTTP/%s" %d',
            "127.0.0.1:32100",
            "GET",
            target,
            "1.1",
            200,
        )
        return stream.getvalue()
    finally:
        logger.handlers = previous_handlers
        logger.filters = previous_filters
        logger.propagate = previous_propagate
        logger.setLevel(previous_level)


def test_actual_uvicorn_037_access_record_is_credential_safe():
    """Pin Uvicorn 0.37's real five-argument AccessFormatter record contract."""
    sentinels = {
        "p": "password-sentinel",
        "t": "token-sentinel",
        "s": "salt-sentinel",
        "apiKey": "api-key-sentinel",
        "Pw": "jellyfin-password-sentinel",
        "token": "jellyfin-token-sentinel",
    }
    target = "/subsonic/rest/ping?" + "&".join(
        f"{key}={value}" for key, value in sentinels.items()
    )
    formatted = _format_real_uvicorn_access_record(target)
    assert all(value not in formatted for value in sentinels.values())
    assert "GET /subsonic/rest/ping?" in formatted
    assert "HTTP/1.1" in formatted
    assert "200 OK" in formatted


def test_uvicorn_filter_ignores_non_access_application_records():
    record = logging.LogRecord(
        "application", logging.INFO, __file__, 1, "safe event", (), None
    )
    assert UvicornAccessCredentialFilter().filter(record) is True
    assert record.msg == "safe event"


def test_form_and_header_credentials_are_absent_from_access_record_shape():
    """Uvicorn's access tuple contains only client/method/target/version/status."""
    form_password = "form-password-sentinel"
    jellyfin_header = "jellyfin-header-sentinel"
    formatted = _format_real_uvicorn_access_record("/subsonic/rest/ping")
    assert form_password not in formatted
    assert jellyfin_header not in formatted


# ----- CORS (dedicated app) -----

def _app() -> TestClient:
    app = FastAPI()
    app.add_middleware(CompatCORSMiddleware)

    @app.get("/subsonic/rest/ping")
    def ping():
        return {"ok": True}

    @app.get("/jellyfin/Audio/x/stream")
    def stream():
        return Response(b"audio")

    return TestClient(app)


def test_cors_preflight_short_circuits_before_auth():
    client = _app()
    r = client.options("/subsonic/rest/ping")
    assert r.status_code == 204
    assert r.headers["Access-Control-Allow-Origin"] == "*"
    assert "GET" in r.headers["Access-Control-Allow-Methods"]
    assert "Authorization" in r.headers["Access-Control-Allow-Headers"]


def test_cors_headers_on_normal_response():
    client = _app()
    r = client.get("/subsonic/rest/ping")
    assert r.status_code == 200
    assert r.headers["Access-Control-Allow-Origin"] == "*"
    assert "Content-Range" in r.headers["Access-Control-Expose-Headers"]
    # no credentials header with wildcard origin
    assert "Access-Control-Allow-Credentials" not in r.headers


def test_streaming_paths_are_exempt():
    client = _app()
    # far more than any browse cap; streaming must never be throttled
    assert all(client.get("/jellyfin/Audio/x/stream").status_code == 200 for _ in range(200))


def test_subsonic_reject_renders_failed_envelope():
    resp = reject_subsonic("json", None, 3)
    body = json.loads(resp.body)["subsonic-response"]
    assert body["status"] == "failed"
    assert body["error"]["code"] == 0
    assert resp.headers["Retry-After"] == "3"


def test_subsonic_binary_reject_is_429_without_body():
    resp = reject_subsonic("json", None, 3, binary=True)
    assert resp.status_code == 429
    assert resp.headers["Retry-After"] == "3"
    assert resp.body == b""


def test_jellyfin_reject_is_429():
    resp = reject_jellyfin(4)
    assert resp.status_code == 429
    assert resp.headers["Retry-After"] == "4"


class _Clock:
    def __init__(self) -> None:
        self.now = 100.0

    def __call__(self) -> float:
        return self.now


@pytest.mark.asyncio
async def test_principal_browse_buckets_are_isolated():
    state = CompatRateLimitState()
    for _ in range(120):
        assert await state.principal_retry_after("user-a", mutation=False) is None
    assert await state.principal_retry_after("user-a", mutation=False) is not None
    assert await state.principal_retry_after("user-b", mutation=False) is None


@pytest.mark.asyncio
async def test_mutation_and_browse_buckets_are_separate():
    state = CompatRateLimitState()
    for _ in range(20):
        assert await state.principal_retry_after("user-a", mutation=True) is None
    assert await state.principal_retry_after("user-a", mutation=True) is not None
    assert await state.principal_retry_after("user-a", mutation=False) is None


def test_auth_failure_limit_is_per_ip_and_escalates():
    state = CompatRateLimitState()
    for _ in range(4):
        assert state.record_auth_failure("192.0.2.1") is None
    assert state.record_auth_failure("192.0.2.1") == 10
    assert state.auth_failure_retry_after("192.0.2.1") == 10
    assert state.auth_failure_retry_after("192.0.2.2") is None


def test_bounded_state_evicts_only_lru_identity():
    clock = _Clock()
    state = CompatRateLimitState(max_ips=2, clock=clock)
    state.record_auth_failure("old")
    clock.now += 1
    state.record_auth_failure("kept")
    clock.now += 1
    state.auth_failure_retry_after("old")
    clock.now += 1
    state.record_auth_failure("new")
    assert state.auth_ip_keys == ("old", "new")


@pytest.mark.asyncio
async def test_principal_state_is_bounded_and_ttl_expired():
    clock = _Clock()
    state = CompatRateLimitState(max_principals=2, ttl_seconds=5, clock=clock)
    await state.principal_retry_after("a", mutation=False)
    clock.now += 1
    await state.principal_retry_after("b", mutation=False)
    clock.now += 1
    await state.principal_retry_after("c", mutation=False)
    assert state.browse_principal_keys == ("b", "c")
    clock.now += 6
    assert state.principal_state_sizes == (0, 0)


def test_raw_forwarded_for_cannot_change_limiter_identity():
    request = SimpleNamespace(
        client=SimpleNamespace(host="198.51.100.9"),
        headers={"x-forwarded-for": "203.0.113.44"},
    )
    assert trusted_client_ip(request) == "198.51.100.9"


# ----- audit log -----

@pytest.mark.asyncio
async def test_create_and_revoke_audit_logged(app_password_service, caplog):
    with caplog.at_level("INFO"):
        record, _secret = await app_password_service.create("user-alice", "Phone")
        await app_password_service.revoke("user-alice", record.id)
    text = caplog.text
    assert "app-password created" in text and "app-password revoked" in text
    # the secret/hash never appears in the audit line
    assert _secret not in text


@pytest.mark.asyncio
async def test_admin_revoke_audit_names_actor_and_owner(app_password_service, caplog):
    record, secret = await app_password_service.create("user-alice", "Phone")
    with caplog.at_level("INFO"):
        await app_password_service.admin_revoke("user-admin", record.id)
    text = caplog.text
    assert "admin-revoked" in text
    assert "admin=user-admin" in text and "owner=user-alice" in text
    assert secret not in text  # never the secret
