"""SlskdClient tests: raw httpx calls against the slskd mock + targeted
MockTransport checks for body shape (searchTimeout ms, plain-array enqueue),
404/429 handling, and the AUD-10 error convention."""

import json

import httpx
import pytest

from core.exceptions import RateLimitedError, SlskdApiError
from repositories.slskd.slskd_client import SlskdClient
from repositories.slskd.slskd_models import SlskdEnqueueResponse
from tests.mocks import slskd_mock


@pytest.fixture
def mock_client() -> SlskdClient:
    slskd_mock.reset_state()
    transport = httpx.ASGITransport(app=slskd_mock.app)
    http = httpx.AsyncClient(transport=transport)
    return SlskdClient(http, "http://slskd", "test-key")


@pytest.mark.asyncio
async def test_health_check_returns_version(mock_client):
    info = await mock_client.health_check()
    assert info["version"]["current"] == "0.25.1.0"


@pytest.mark.asyncio
async def test_start_search_completes(mock_client):
    search = await mock_client.start_search("Radiohead - OK Computer", timeout_seconds=5)
    assert search.id
    state = await mock_client.get_search_state(search.id)
    assert state.is_complete is True


@pytest.mark.asyncio
async def test_search_responses_parse_lossless(mock_client):
    search = await mock_client.start_search("q", timeout_seconds=5)
    responses = await mock_client.get_search_responses(search.id)
    alice = next(r for r in responses if r.username == "alice")
    assert len(alice.files) == 12
    # bitRate is ABSENT for lossless -> None, not 0 (C6b).
    assert alice.files[0].bit_rate is None
    assert alice.files[0].bit_depth == 16


@pytest.mark.asyncio
async def test_enqueue_returns_enqueued_failed(mock_client):
    resp = await mock_client.enqueue("alice", [{"filename": "a.flac", "size": 100}])
    assert isinstance(resp, SlskdEnqueueResponse)
    assert len(resp.enqueued) == 1
    assert resp.failed == []


@pytest.mark.asyncio
async def test_get_downloads_reflects_enqueue(mock_client):
    await mock_client.enqueue("alice", [{"filename": "dir/a.flac", "size": 100}])
    transfers = await mock_client.get_downloads("alice")
    assert any(t.filename == "dir/a.flac" for t in transfers)


@pytest.mark.asyncio
async def test_get_downloads_unknown_user_empty(mock_client):
    transfers = await mock_client.get_downloads("nobody")
    assert transfers == []


@pytest.mark.asyncio
async def test_cancel_transfer_removes(mock_client):
    await mock_client.enqueue("alice", [{"filename": "dir/a.flac", "size": 100}])
    transfer_id = (await mock_client.get_downloads("alice"))[0].id
    assert await mock_client.cancel_transfer("alice", transfer_id) is True
    assert all(t.id != transfer_id for t in await mock_client.get_downloads("alice"))


@pytest.mark.asyncio
async def test_missing_api_key_raises(mock_client):
    # Search calls are not retry-wrapped -> the 401 surfaces immediately.
    transport = httpx.ASGITransport(app=slskd_mock.app)
    http = httpx.AsyncClient(transport=transport)
    client = SlskdClient(http, "http://slskd", "")  # empty key -> mock 401
    with pytest.raises(SlskdApiError):
        await client.start_search("q", timeout_seconds=1)


@pytest.mark.asyncio
async def test_search_timeout_is_milliseconds():
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, json={"id": "s1", "state": "InProgress", "isComplete": False})

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    await client.start_search("q", timeout_seconds=30.0)
    assert captured["body"]["searchTimeout"] == 30000


@pytest.mark.asyncio
async def test_enqueue_sends_plain_array():
    captured: dict = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(201, json={"Enqueued": [{"filename": "a", "size": 1}], "Failed": []})

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    await client.enqueue("alice", [{"filename": "a", "size": 1}])
    assert isinstance(captured["body"], list)
    assert captured["body"][0]["filename"] == "a"
    assert "options" not in str(captured["body"])


@pytest.mark.asyncio
async def test_get_options_parses_directories():
    # Shape verified against a live slskd: directories is top-level with downloads/incomplete.
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith("/api/v0/options")
        return httpx.Response(200, json={
            "directories": {
                "incomplete": "/data/downloads/slskd_incomplete",
                "downloads": "/data/downloads/slskd",
            },
            "someOtherField": 123,  # unknown fields must be ignored
        })

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    options = await client.get_options()
    assert options.directories.downloads == "/data/downloads/slskd"
    assert options.directories.incomplete == "/data/downloads/slskd_incomplete"


@pytest.mark.asyncio
async def test_429_raises_rate_limited():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(429, text="Only one concurrent operation is permitted")

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    with pytest.raises(RateLimitedError):
        await client.enqueue("alice", [{"filename": "a", "size": 1}])


@pytest.mark.asyncio
async def test_get_downloads_404_returns_empty():
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(404)

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    client = SlskdClient(http, "http://slskd", "k")
    assert await client.get_downloads("ghost") == []
