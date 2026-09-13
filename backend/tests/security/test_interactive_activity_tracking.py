from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from starlette.requests import Request
from starlette.responses import Response

from middleware import AuthMiddleware


def _request(path: str) -> Request:
    return Request(
        {
            "type": "http",
            "method": "GET",
            "path": path,
            "raw_path": path.encode(),
            "query_string": b"",
            "headers": [(b"authorization", b"Bearer test-token")],
            "client": ("127.0.0.1", 1234),
            "server": ("testserver", 80),
            "scheme": "http",
        }
    )


@pytest.mark.asyncio
async def test_authenticated_api_request_records_interactive_activity() -> None:
    auth = MagicMock()
    auth.verify_token = AsyncMock(
        return_value=(SimpleNamespace(id="user-1"), SimpleNamespace(id="token-1"))
    )
    gate = MagicMock()
    middleware = AuthMiddleware(MagicMock())

    with (
        patch("core.dependencies.auth_providers.get_auth_service", return_value=auth),
        patch(
            "core.dependencies.service_providers.get_background_workload_gate",
            return_value=gate,
        ),
    ):
        response = await middleware.dispatch(
            _request("/api/v1/library/albums"), AsyncMock(return_value=Response())
        )

    assert response.status_code == 200
    gate.begin_interactive_request.assert_called_once_with()
    gate.end_interactive_request.assert_called_once_with()


@pytest.mark.asyncio
async def test_interactive_activity_hold_is_released_when_handler_fails() -> None:
    auth = MagicMock()
    auth.verify_token = AsyncMock(
        return_value=(SimpleNamespace(id="user-1"), SimpleNamespace(id="token-1"))
    )
    gate = MagicMock()
    middleware = AuthMiddleware(MagicMock())

    with (
        patch("core.dependencies.auth_providers.get_auth_service", return_value=auth),
        patch(
            "core.dependencies.service_providers.get_background_workload_gate",
            return_value=gate,
        ),
        pytest.raises(RuntimeError, match="handler failed"),
    ):
        await middleware.dispatch(
            _request("/api/v1/search?q=test"),
            AsyncMock(side_effect=RuntimeError("handler failed")),
        )

    gate.begin_interactive_request.assert_called_once_with()
    gate.end_interactive_request.assert_called_once_with()


@pytest.mark.parametrize(
    "path",
    [
        "/api/v1/stream/local/file-id",
        "/api/v1/downloads/task-id/stream",
        "/api/v1/library/scan/stream",
        "/api/v1/events/stream",
        "/api/v1/downloads/task-id/held-audio/file-id",
    ],
)
def test_streaming_paths_do_not_count_as_interactive_activity(path: str) -> None:
    assert AuthMiddleware._tracks_interactive_activity(path) is False


def test_regular_api_path_counts_as_interactive_activity() -> None:
    assert AuthMiddleware._tracks_interactive_activity("/api/v1/search?q=test") is True
