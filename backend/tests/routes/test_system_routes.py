"""QW9 Parts 1+3: auth matrix and shapes for /system/queue-stats and
/system/provider-stats (both admin-gated)."""

from fastapi import FastAPI, HTTPException

from api.v1.routes import system as system_routes
from infrastructure.cache.cache_metrics import WindowedCounterMap
from infrastructure.observability import provider_counters
from infrastructure.observability.provider_counters import RateLimitGauge
from repositories.musicbrainz_base import MbSourceContext
from infrastructure.queue.priority_queue import get_priority_queue
from middleware import _get_current_admin
from tests.helpers import build_test_client, mock_admin_user


def _deny_admin() -> None:
    raise HTTPException(status_code=403, detail="admin only")


def _app(admin_override) -> FastAPI:
    app = FastAPI()
    app.include_router(system_routes.router)
    if admin_override is not None:
        app.dependency_overrides[_get_current_admin] = admin_override
    return app


class TestRoutePaths:
    def test_gauge_paths_registered(self):
        paths = [route.path for route in system_routes.router.routes]
        assert "/system/queue-stats" in paths
        assert "/system/provider-stats" in paths


class TestQueueStatsAuthMatrix:
    def test_unauthenticated_gets_401(self):
        response = build_test_client(_app(None)).get("/system/queue-stats")
        assert response.status_code == 401

    def test_non_admin_gets_403(self):
        response = build_test_client(_app(_deny_admin)).get("/system/queue-stats")
        assert response.status_code == 403

    def test_admin_gets_queue_snapshot(self):
        response = build_test_client(_app(mock_admin_user)).get("/system/queue-stats")

        assert response.status_code == 200
        body = response.json()
        stats = get_priority_queue().get_stats()
        assert body["stats"] == {
            "user_slots_available": stats["user_slots_available"],
            "image_slots_available": stats["image_slots_available"],
            "background_slots_available": stats["background_slots_available"],
            "user_active": stats["user_active"],
            "background_waiters": stats["background_waiters"],
        }


class TestProviderStatsAuthMatrix:
    def test_unauthenticated_gets_401(self):
        response = build_test_client(_app(None)).get("/system/provider-stats")
        assert response.status_code == 401

    def test_non_admin_gets_403(self):
        response = build_test_client(_app(_deny_admin)).get("/system/provider-stats")
        assert response.status_code == 403

    def test_admin_gets_envelope_shape(self, monkeypatch):
        monkeypatch.setattr(provider_counters, "_counters", WindowedCounterMap())
        monkeypatch.setattr(provider_counters, "_rate_limit_gauge", RateLimitGauge())

        response = build_test_client(_app(mock_admin_user)).get(
            "/system/provider-stats"
        )

        assert response.status_code == 200
        body = response.json()
        assert body["window_seconds"] == 3600
        assert isinstance(body["providers"], list)
        # QW11 Part 2: additive telemetry envelope, defaulted empty
        assert body["rate_limits"] == []

    def test_admin_gets_rate_limit_rows_when_observed(self, monkeypatch):
        gauge = RateLimitGauge()

        class _Headers:
            def get(self, name):
                return {
                    "x-ratelimit-limit": "15",
                    "x-ratelimit-remaining": "14",
                    "x-ratelimit-reset": "1787600000",
                    "x-mb-rate-limiter": "lua",
                }.get(name)

        monkeypatch.setattr(provider_counters, "_rate_limit_gauge", gauge)
        provider_counters.record_rate_limit_headers("musicbrainz", _Headers())

        response = build_test_client(_app(mock_admin_user)).get(
            "/system/provider-stats"
        )

        assert response.status_code == 200
        rate_limits = response.json()["rate_limits"]
        assert rate_limits == [
            {
                "provider": "musicbrainz",
                "limit": 15,
                "remaining": 14,
                "reset_epoch": 1787600000.0,
                "observed_at": rate_limits[0]["observed_at"],
                "low_remaining_events_window": 0,
            }
        ]


class TestProviderStatsRows:
    def test_rows_render_recorded_calls(self, monkeypatch):
        fresh = WindowedCounterMap()
        fresh.increment(("listenbrainz", "unlaned", "ok"), 12)
        monkeypatch.setattr(provider_counters, "_counters", fresh)

        response = build_test_client(_app(mock_admin_user)).get(
            "/system/provider-stats"
        )

        assert response.status_code == 200
        providers = response.json()["providers"]
        assert providers == [
            {
                "provider": "listenbrainz",
                "priority": "unlaned",
                "outcome": "ok",
                "count_total": 12,
                "rate_per_min_window": round(12 / 60, 2),
            }
        ]

    def test_admin_gets_source_dimensions_on_musicbrainz_rows(self, monkeypatch):
        fresh = WindowedCounterMap()
        monkeypatch.setattr(provider_counters, "_counters", fresh)
        provider_counters.record_provider_call(
            "musicbrainz",
            "background_sync",
            200,
            source_context=MbSourceContext(
                source_url="https://mirror.example/ws/2",
                generation=4,
                source_mode="mirror",
                source_id="mirror-4",
            ),
        )

        response = build_test_client(_app(mock_admin_user)).get(
            "/system/provider-stats"
        )

        assert response.status_code == 200
        assert response.json()["providers"] == [
            {
                "provider": "musicbrainz",
                "priority": "background_sync",
                "outcome": "ok",
                "count_total": 1,
                "rate_per_min_window": round(1 / 60, 2),
                "source_mode": "mirror",
                "source_id": "mirror-4",
                "source_generation": 4,
            }
        ]
