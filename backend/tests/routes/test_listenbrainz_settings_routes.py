from unittest.mock import AsyncMock, MagicMock

from fastapi import FastAPI

from api.v1.routes import settings as settings_routes
from core.dependencies import get_settings_service
from core.exceptions import RateLimitedError
from middleware import _get_current_admin
from tests.helpers import build_test_client


def test_listenbrainz_verification_rate_limit_returns_safe_429():
    settings_service = MagicMock()
    provider_message = "provider body sentinel"
    credential = "listenbrainz credential sentinel"
    settings_service.verify_listenbrainz = AsyncMock(
        side_effect=RateLimitedError(
            provider_message,
            details={"credential": credential},
            retry_after_seconds=17,
        )
    )

    app = FastAPI()
    app.include_router(settings_routes.router)
    app.dependency_overrides[get_settings_service] = lambda: settings_service
    app.dependency_overrides[_get_current_admin] = lambda: None

    response = build_test_client(app).post(
        "/settings/listenbrainz/verify",
        json={"username": "alice_lb", "user_token": credential, "enabled": True},
    )

    assert response.status_code == 429
    error = response.json()["error"]
    assert error["code"] == "RATE_LIMITED"
    assert (
        error["message"]
        == "ListenBrainz is temporarily rate-limiting this server. Try again shortly."
    )
    assert error["details"] is None
    assert provider_message not in response.text
    assert credential not in response.text
    settings_service.verify_listenbrainz.assert_awaited_once()
