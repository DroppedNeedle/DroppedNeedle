import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from unittest.mock import AsyncMock, MagicMock

from api.v1.routes import settings as settings_routes
from api.v1.schemas.settings import (
    BRAINZMASH_DISCLOSURE_VERSION,
    BRAINZMASH_ENDPOINT,
    BrainzMashActiveBinding,
    BrainzMashPendingProposal,
    MusicBrainzBindingRequest,
    MusicBrainzConnectionSettings,
)
from core.dependencies import get_preferences_service, get_settings_service
from middleware import _get_current_admin
from infrastructure.cache.memory_cache import InMemoryCache
from repositories import musicbrainz_base as mb_base
from services.settings_service import MusicBrainzVerifyResult, SettingsService


def _build_app(current, *, preferences_service=None, settings_service=None):
    app = FastAPI()
    app.include_router(settings_routes.router)
    preferences_service = preferences_service or MagicMock()
    settings_service = settings_service or MagicMock()
    preferences_service.get_musicbrainz_connection.return_value = current

    async def override_preferences():
        return preferences_service

    async def override_settings():
        return settings_service

    async def override_admin():
        return None

    app.dependency_overrides[get_preferences_service] = override_preferences
    app.dependency_overrides[get_settings_service] = override_settings
    app.dependency_overrides[_get_current_admin] = override_admin
    return app, preferences_service, settings_service


def _current(source_mode="official", api_url="https://musicbrainz.org/ws/2"):
    return MusicBrainzConnectionSettings(
        source_mode=source_mode,
        selected_source_mode=source_mode,
        api_url=api_url,
        source_id="current-source",
        generation=4,
        community_acknowledged=source_mode == "community",
    )


@pytest.mark.parametrize(
    ("source_mode", "api_url", "acknowledged"),
    [
        ("official", None, None),
        ("mirror", "https://mirror.example/ws/2", None),
        ("community", "https://community.example/ws/2", True),
    ],
    ids=["official", "mirror", "community"],
)
def test_non_brainzmash_verification_uses_unsaved_draft(
    source_mode, api_url, acknowledged
):
    current = _current()
    preferences_service = MagicMock()
    settings_service = MagicMock()
    settings_service.verify_musicbrainz = AsyncMock(
        return_value=MusicBrainzVerifyResult(valid=True, message="connected")
    )
    app, preferences_service, settings_service = _build_app(
        current,
        preferences_service=preferences_service,
        settings_service=settings_service,
    )

    payload = {
        "source_mode": source_mode,
        "rate_limit": 2.0,
        "concurrent_searches": 5,
    }
    if api_url is not None:
        payload["api_url"] = api_url
    if acknowledged is not None:
        payload["community_acknowledged"] = acknowledged
    else:
        payload["community_acknowledged"] = None

    response = TestClient(app).post("/settings/musicbrainz/verify", json=payload)

    assert response.status_code == 200
    settings_service.verify_musicbrainz.assert_awaited_once()
    draft = settings_service.verify_musicbrainz.await_args.args[0]
    assert draft.source_mode == source_mode
    assert draft.api_url == (api_url or "https://musicbrainz.org/ws/2")
    assert draft.rate_limit == (1.0 if source_mode == "official" else 2.0)
    assert draft.concurrent_searches == 5
    assert draft.community_acknowledged is bool(acknowledged)
    preferences_service.save_musicbrainz_update.assert_not_called()
    preferences_service.save_musicbrainz_connection.assert_not_called()


def test_brainzmash_staging_uses_settings_service_transition():
    current = _current()
    app, _, settings_service = _build_app(current)
    settings_service.stage_brainzmash = AsyncMock(return_value=current)

    response = TestClient(app).post("/settings/musicbrainz/brainzmash/stage")

    assert response.status_code == 200
    settings_service.stage_brainzmash.assert_awaited_once_with()


def test_brainzmash_verification_requires_exact_binding_and_records_only_after_success():
    pending = BrainzMashPendingProposal(
        endpoint=BRAINZMASH_ENDPOINT,
        access_revision="revision-1",
        source_id="pending-source",
        generation=5,
        disclosure_version=BRAINZMASH_DISCLOSURE_VERSION,
        consented=True,
    )
    current = MusicBrainzConnectionSettings(
        source_mode="official",
        selected_source_mode="brainzmash",
        source_id="official-source",
        generation=4,
        pending_brainzmash=pending,
    )
    preferences_service = MagicMock()
    settings_service = MagicMock()
    settings_service.verify_brainzmash = AsyncMock(
        return_value=MusicBrainzVerifyResult(valid=True, message="connected")
    )
    preferences_service.record_brainzmash_verification.return_value = current
    app, preferences_service, settings_service = _build_app(
        current,
        preferences_service=preferences_service,
        settings_service=settings_service,
    )
    binding = {
        "access_revision": pending.access_revision,
        "source_id": pending.source_id,
        "generation": pending.generation,
        "disclosure_version": pending.disclosure_version,
    }

    response = TestClient(app).post("/settings/musicbrainz/verify", json=binding)

    assert response.status_code == 200
    settings_service.verify_brainzmash.assert_awaited_once_with(
        MusicBrainzBindingRequest(**binding)
    )
    preferences_service.record_brainzmash_verification.assert_called_once_with(
        MusicBrainzBindingRequest(**binding)
    )
    settings_service.verify_musicbrainz.assert_not_called()
    preferences_service.save_musicbrainz_update.assert_not_called()


def test_active_brainzmash_rejects_alternative_draft_verification_without_switching():
    current = MusicBrainzConnectionSettings(
        source_mode="brainzmash",
        selected_source_mode="brainzmash",
        api_url=BRAINZMASH_ENDPOINT.rstrip("/"),
        source_id="active-brainzmash",
        generation=9,
        active_brainzmash=BrainzMashActiveBinding(
            endpoint=BRAINZMASH_ENDPOINT.rstrip("/"),
            access_revision="active-revision",
            source_id="active-brainzmash",
            generation=9,
            disclosure_version=BRAINZMASH_DISCLOSURE_VERSION,
            consented=True,
            verified=True,
        ),
    )
    preferences_service = MagicMock()
    settings_service = MagicMock()
    settings_service.verify_musicbrainz = AsyncMock(
        return_value=MusicBrainzVerifyResult(valid=True, message="connected")
    )
    app, preferences_service, settings_service = _build_app(
        current,
        preferences_service=preferences_service,
        settings_service=settings_service,
    )
    before = mb_base.capture_mb_source_context()
    mb_base.set_mb_api_base(
        current.api_url,
        source_mode=current.source_mode,
        source_id=current.source_id,
        generation=current.generation,
        brainzmash_binding_valid=True,
    )
    try:
        response = TestClient(app).post(
            "/settings/musicbrainz/verify",
            json={
                "source_mode": "official",
                "api_url": None,
                "rate_limit": 1.0,
                "concurrent_searches": 1,
                "community_acknowledged": None,
            },
        )
        assert response.status_code == 409
        settings_service.verify_musicbrainz.assert_not_awaited()
        assert preferences_service.save_musicbrainz_update.call_count == 0
        assert preferences_service.save_musicbrainz_connection.call_count == 0
        assert mb_base.get_mb_api_base() == current.api_url
        assert mb_base.get_mb_source_mode() == "brainzmash"
        assert mb_base.get_mb_source_id() == current.source_id
        assert mb_base.get_mb_source_generation() == current.generation
    finally:
        mb_base.set_mb_api_base(
            before.source_url,
            source_mode=before.source_mode,
            source_id=before.source_id,
            generation=before.generation,
        )


def test_quarantined_brainzmash_allows_alternative_draft_verification():
    current = MusicBrainzConnectionSettings(
        source_mode="brainzmash",
        selected_source_mode="official",
        api_url=BRAINZMASH_ENDPOINT.rstrip("/"),
        source_id="quarantined-brainzmash",
        generation=9,
        source_quarantined=True,
        quarantine_reason="invalid binding",
    )
    preferences_service = MagicMock()
    settings_service = MagicMock()
    settings_service.verify_musicbrainz = AsyncMock(
        return_value=MusicBrainzVerifyResult(valid=True, message="connected")
    )
    app, preferences_service, settings_service = _build_app(
        current,
        preferences_service=preferences_service,
        settings_service=settings_service,
    )
    before = mb_base.capture_mb_source_context()
    before_runtime = mb_base.brainzmash_runtime_enabled()
    mb_base.set_mb_api_base(
        current.api_url,
        source_mode="brainzmash",
        source_id=current.source_id,
        generation=current.generation,
        brainzmash_binding_valid=False,
    )
    try:
        response = TestClient(app).post(
            "/settings/musicbrainz/verify",
            json={
                "source_mode": "official",
                "api_url": None,
                "rate_limit": 1.0,
                "concurrent_searches": 1,
                "community_acknowledged": None,
            },
        )
    finally:
        mb_base.set_mb_api_base(
            before.source_url,
            source_mode=before.source_mode,
            source_id=before.source_id,
            generation=before.generation,
            brainzmash_binding_valid=before_runtime,
        )

    assert response.status_code == 200
    settings_service.verify_musicbrainz.assert_awaited_once()


def test_brainzmash_verification_rejects_preconsent_without_probe(monkeypatch):
    pending = BrainzMashPendingProposal(
        endpoint=BRAINZMASH_ENDPOINT,
        access_revision="revision-1",
        source_id="pending-source",
        generation=5,
        disclosure_version=BRAINZMASH_DISCLOSURE_VERSION,
        consented=False,
    )
    current = MusicBrainzConnectionSettings(
        source_mode="official",
        selected_source_mode="brainzmash",
        pending_brainzmash=pending,
    )
    preferences_service = MagicMock()
    preferences_service.get_musicbrainz_connection.return_value = current
    settings_service = SettingsService(
        preferences_service=preferences_service, cache=InMemoryCache()
    )
    probe = AsyncMock()
    monkeypatch.setattr(mb_base, "mb_api_probe", probe)
    app, _, _ = _build_app(
        current,
        preferences_service=preferences_service,
        settings_service=settings_service,
    )

    response = TestClient(app).post(
        "/settings/musicbrainz/verify",
        json={
            "access_revision": pending.access_revision,
            "source_id": pending.source_id,
            "generation": pending.generation,
            "disclosure_version": pending.disclosure_version,
        },
    )

    assert response.status_code == 409
    assert "consent" in response.json()["detail"].lower()
    probe.assert_not_awaited()
    preferences_service.record_brainzmash_verification.assert_not_called()


def test_brainzmash_update_payload_is_rejected_without_verifier():
    current = _current()
    preferences_service = MagicMock()
    settings_service = MagicMock()
    settings_service.verify_musicbrainz = AsyncMock()
    app, _, settings_service = _build_app(
        current,
        preferences_service=preferences_service,
        settings_service=settings_service,
    )

    response = TestClient(app).post(
        "/settings/musicbrainz/verify",
        json={
            "source_mode": "brainzmash",
            "api_url": "https://musicbrainz.org/ws/2",
            "rate_limit": 1.0,
            "concurrent_searches": 1,
        },
    )

    assert response.status_code == 422
    settings_service.verify_musicbrainz.assert_not_called()


def test_musicbrainz_verification_requires_admin():
    app = FastAPI()
    app.include_router(settings_routes.router)
    response = TestClient(app).post(
        "/settings/musicbrainz/verify",
        json={
            "source_mode": "official",
            "api_url": "https://musicbrainz.org/ws/2",
            "rate_limit": 1.0,
            "concurrent_searches": 1,
        },
    )

    assert response.status_code == 401
