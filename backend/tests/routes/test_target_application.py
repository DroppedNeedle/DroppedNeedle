import ast
import logging
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from core.dependencies import (
    get_acquisition_dispatcher,
    get_album_discovery_service,
    get_album_service,
    get_artist_discovery_service,
    get_artist_service,
    get_discover_queue_manager,
    get_discovery_batch_service,
    get_download_service,
    get_drop_import_service,
    get_events_watcher_getter,
    get_free_music_service,
    get_home_charts_service,
    get_navidrome_library_service,
    get_plex_library_service,
    get_settings_service,
    get_scrobble_service,
    get_personal_mix_service,
    get_quota_service,
    get_requests_page_service,
    get_spotify_import_service,
    get_cache_service,
    get_coverart_repository,
    get_wrapped_service,
    get_target_acquisition_dispatcher,
    get_target_album_discovery_service,
    get_target_album_service,
    get_target_artist_discovery_service,
    get_target_artist_service,
    get_target_discover_queue_manager,
    get_target_discovery_batch_service,
    get_target_download_service,
    get_target_drop_import_service,
    get_target_events_watcher_service,
    get_target_free_music_service,
    get_target_home_charts_service,
    get_target_navidrome_library_service,
    get_target_plex_library_service,
    get_target_settings_service,
    get_target_personal_mix_service,
    get_target_quota_service,
    get_target_library_ownership_service,
    get_target_native_library_service,
    get_target_requests_page_service,
    get_target_spotify_import_service,
    get_target_cache_service,
    get_target_wrapped_service,
    get_target_library_policy_service,
    get_target_wanted_watcher_service,
    get_wanted_watcher_service,
)
from core.dependencies import service_providers
from core.dependencies import repo_providers
from core.exceptions import ProviderIdentityRequiredError, TargetStartupInvariantError
from services.album_discovery_service import AlbumDiscoveryService
from services.compat.target_cover_art_service import TargetCoverArtService
from api.v1.schemas.library_policies import LibrarySettingsResponse
from target_application import (
    _server_timezone_name,
    create_isolated_target_application,
    create_production_target_application,
)
from tests.helpers import build_test_client, override_admin_auth, override_user_auth


def test_target_scheduler_uses_configured_iana_timezone(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("TZ", "Europe/London")

    assert _server_timezone_name() == "Europe/London"


def test_library_operation_stream_precedes_dynamic_operation_route() -> None:
    app = create_isolated_target_application()
    paths = [route.path for route in app.routes]

    assert paths.index("/api/v1/library/operations/stream") < paths.index(
        "/api/v1/library/operations/{job_id}"
    )


@pytest.mark.parametrize("invalid_timezone", ["BST", "/etc/localtime"])
def test_target_scheduler_rejects_invalid_timezone_and_falls_back_to_utc(
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
    invalid_timezone: str,
) -> None:
    import target_application as target_module

    class UnexpectedDateTime:
        @classmethod
        def now(cls):
            raise AssertionError("invalid configured TZ must not use a host-local zone")

    monkeypatch.setenv("TZ", invalid_timezone)
    monkeypatch.setattr(target_module, "datetime", UnexpectedDateTime)

    assert _server_timezone_name() == "UTC"
    assert "scheduled scans will use UTC" in caplog.text


def test_target_scheduler_uses_local_iana_key_when_tz_is_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import target_application as target_module

    class KeyedDateTime:
        @classmethod
        def now(cls):
            return SimpleNamespace(
                astimezone=lambda: SimpleNamespace(
                    tzinfo=SimpleNamespace(key="America/New_York")
                )
            )

    monkeypatch.delenv("TZ", raising=False)
    monkeypatch.setattr(target_module, "datetime", KeyedDateTime)

    assert _server_timezone_name() == "America/New_York"


def test_isolated_target_application_mounts_target_catalog_and_compat_routes() -> None:
    native = AsyncMock()
    native.albums.return_value = ([], 0)
    app = create_isolated_target_application(
        dependency_overrides={get_target_native_library_service: lambda: native}
    )
    override_user_auth(app, role="user")

    response = build_test_client(app).get("/api/v1/library/albums")
    route_modules = {
        route.endpoint.__module__ for route in app.routes if hasattr(route, "endpoint")
    }

    assert response.status_code == 200
    assert response.json() == {"items": [], "total": 0}
    assert "api.v1.routes.library_target" in route_modules
    assert "api.v1.routes.library_scan_target" in route_modules
    assert "api.v1.routes.library_operations_target" in route_modules
    assert "api.v1.routes.library_management" in route_modules
    assert "api.v1.routes.library" not in route_modules
    assert "api.v1.routes.library_scan" not in route_modules
    assert "api.compat.subsonic.router" in route_modules
    assert "api.compat.jellyfin.router" in route_modules
    assert {
        "api.v1.routes.stream",
        "api.v1.routes.local_library",
        "api.v1.routes.download",
        "api.v1.routes.downloads",
        "api.v1.routes.downloads_search",
        "api.v1.routes.import_drop",
        "api.v1.routes.free_music",
        "api.v1.routes.tracks",
        "api.v1.routes.requests_page",
        "api.v1.routes.scrobble",
    }.issubset(route_modules)
    assert app.dependency_overrides[get_download_service] is get_target_download_service
    assert app.dependency_overrides[get_album_service] is get_target_album_service
    assert app.dependency_overrides[get_artist_service] is get_target_artist_service
    assert (
        app.dependency_overrides[get_artist_discovery_service]
        is get_target_artist_discovery_service
    )
    assert (
        app.dependency_overrides[get_album_discovery_service]
        is get_target_album_discovery_service
    )
    assert (
        app.dependency_overrides[get_home_charts_service]
        is get_target_home_charts_service
    )
    assert (
        app.dependency_overrides[get_discover_queue_manager]
        is get_target_discover_queue_manager
    )
    assert (
        app.dependency_overrides[get_acquisition_dispatcher]
        is get_target_acquisition_dispatcher
    )
    assert (
        app.dependency_overrides[get_drop_import_service]
        is get_target_drop_import_service
    )
    assert (
        app.dependency_overrides[get_free_music_service]
        is get_target_free_music_service
    )
    assert (
        app.dependency_overrides[get_requests_page_service]
        is get_target_requests_page_service
    )
    assert (
        app.dependency_overrides[get_wanted_watcher_service]
        is get_target_wanted_watcher_service
    )
    assert (
        app.dependency_overrides[get_discovery_batch_service]
        is get_target_discovery_batch_service
    )
    assert (
        app.dependency_overrides[get_personal_mix_service]
        is get_target_personal_mix_service
    )
    assert app.dependency_overrides[get_quota_service] is get_target_quota_service
    assert (
        app.dependency_overrides[get_spotify_import_service]
        is get_target_spotify_import_service
    )
    assert app.dependency_overrides[get_cache_service] is get_target_cache_service
    assert app.dependency_overrides[get_wrapped_service] is get_target_wrapped_service
    assert (
        app.dependency_overrides[get_navidrome_library_service]
        is get_target_navidrome_library_service
    )
    assert (
        app.dependency_overrides[get_plex_library_service]
        is get_target_plex_library_service
    )
    assert app.dependency_overrides[get_settings_service] is get_target_settings_service
    assert app.dependency_overrides[get_events_watcher_getter]() is (
        get_target_events_watcher_service
    )


def test_target_release_cover_warming_uses_the_target_adapter_surface() -> None:
    release_id = "55555555-5555-4555-8555-555555555555"
    provider = AsyncMock()
    provider.get_release_cover.return_value = None
    provider.is_release_cover_warming = MagicMock(return_value=True)
    covers = TargetCoverArtService(AsyncMock(), provider, AsyncMock())
    app = create_isolated_target_application(
        target_composition=SimpleNamespace(covers=covers)
    )

    assert app.dependency_overrides[get_coverart_repository]() is covers

    response = build_test_client(app).get(f"/api/v1/covers/release/{release_id}")

    assert response.status_code == 202
    assert response.headers["x-cover-source"] == "warming"
    provider.is_release_cover_warming.assert_called_once_with(release_id)


def test_target_native_scrobble_routes_receive_the_native_service(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import target_application as target_module

    native_scrobble = object()
    compat_scrobble = object()
    monkeypatch.setattr(
        target_module,
        "get_target_consumer_composition",
        lambda: SimpleNamespace(
            scrobble_service=native_scrobble,
            scrobble=compat_scrobble,
        ),
    )

    app = create_isolated_target_application()

    assert app.dependency_overrides[get_scrobble_service]() is native_scrobble


def test_target_application_exposes_only_typed_library_root_mutations() -> None:
    policies = AsyncMock()
    policies.get_settings.return_value = LibrarySettingsResponse(
        policy_revision="typed-policy-revision"
    )
    app = create_isolated_target_application(
        dependency_overrides={get_target_library_policy_service: lambda: policies}
    )
    override_admin_auth(app)

    response = build_test_client(app).get("/api/v1/settings/library")
    method_paths = [
        (method, route.path)
        for route in app.routes
        for method in getattr(route, "methods", set())
        if method in {"GET", "PUT", "POST", "DELETE"}
    ]

    assert response.status_code == 200
    assert response.json()["policy_revision"] == "typed-policy-revision"
    assert method_paths.count(("GET", "/api/v1/settings/library")) == 1
    assert method_paths.count(("PUT", "/api/v1/settings/library")) == 1
    assert ("POST", "/api/v1/settings/library/paths") not in method_paths
    assert ("DELETE", "/api/v1/settings/library/paths") not in method_paths
    assert ("GET", "/api/v1/settings/library/path-mapping") not in method_paths
    assert method_paths.count(("GET", "/api/v1/settings/library-management")) == 1
    assert method_paths.count(("POST", "/api/v1/library/management/previews")) == 1
    policies.get_settings.assert_awaited_once()


def test_deployed_entrypoint_has_no_target_selector_or_target_mount() -> None:
    """F-NL-03 cutover: the legacy main:app entrypoint is an unsupported-install
    guard - a stale launcher must fail with upgrade guidance, never serve a
    partial legacy API, and the target source keeps its single composition."""
    import subprocess
    import sys

    backend = Path(__file__).parents[2]
    main_source = (backend / "main.py").read_text()
    assert "APIRouter" not in main_source
    assert "include_router" not in main_source
    assert "FastAPI(" not in main_source
    assert "target_main:app" in main_source  # operator guidance present
    result = subprocess.run(
        [sys.executable, "-c", "import main"],
        cwd=backend,
        capture_output=True,
        text=True,
        env={"PATH": "/usr/bin:/bin", "PYTHONPATH": str(backend)},
    )
    assert result.returncode != 0
    assert "Unsupported installation" in (result.stderr + result.stdout)
    assert "target_main:app" in (result.stderr + result.stdout)
    target_source = (backend / "target_application.py").read_text()
    module = ast.parse(target_source)
    assert not any(
        isinstance(node, (ast.Assign, ast.AnnAssign))
        and any(
            isinstance(target, ast.Name) and target.id == "app"
            for target in (
                node.targets if isinstance(node, ast.Assign) else [node.target]
            )
        )
        for node in module.body
    )
    assert "create_isolated_target_application" in target_source
    assert "runtime selector" not in target_source.casefold()


def test_offline_replacement_entrypoint_is_complete_and_single_worker() -> None:
    backend = Path(__file__).parents[2]
    app = create_production_target_application()
    route_modules = {
        route.endpoint.__module__ for route in app.routes if hasattr(route, "endpoint")
    }
    middleware = {item.cls.__name__ for item in app.user_middleware}

    assert {
        "api.v1.routes.auth",
        "api.v1.routes.system",
        "api.v1.routes.library_target",
        "api.v1.routes.library_scan_target",
        "api.v1.routes.library_operations_target",
        "api.v1.routes.library_management",
        "api.compat.subsonic.router",
        "api.compat.jellyfin.router",
    }.issubset(route_modules)
    assert {
        "AuthMiddleware",
        "CompressibleGZipMiddleware",
        "DegradationMiddleware",
        "PerformanceMiddleware",
        "RateLimitMiddleware",
        "ProxyHeadersMiddleware",
    }.issubset(middleware)
    assert "api.v1.routes.library" not in route_modules
    dockerfile = (backend.parent / "Dockerfile").read_text()
    assert (
        'CMD ["python", "-m", "maintenance.automatic_upgrade", "--start-target"]'
        in dockerfile
    )
    from maintenance.automatic_upgrade import _target_command

    assert _target_command(8688)[-2:] == ["--workers", "1"]


def test_production_target_application_always_runs_startup_validation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN", raising=False)
    reject = AsyncMock(
        side_effect=TargetStartupInvariantError("target migration validation failed")
    )
    monkeypatch.setattr("target_application.TargetStartupValidator.validate", reject)
    app = create_production_target_application()

    with pytest.raises(
        TargetStartupInvariantError, match="target migration validation failed"
    ):
        with build_test_client(app):
            pass
    reject.assert_awaited_once_with("steady_state")


def test_target_lifecycle_retains_every_source_task() -> None:
    """F-NL-03 cutover: the legacy main.py composition is gone; the target
    lifecycle is the sole source of long-lived tasks and retains every
    non-legacy starter (the two legacy scan tasks are removed, not replaced)."""
    backend = Path(__file__).parents[2]

    def starter_calls(path: Path, functions: set[str]) -> set[str]:
        module = ast.parse(path.read_text())
        return {
            call.func.id
            for node in module.body
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name in functions
            for call in ast.walk(node)
            if isinstance(call, ast.Call)
            and isinstance(call.func, ast.Name)
            and call.func.id.startswith("start_")
        }

    target = starter_calls(
        backend / "target_application.py", {"production_target_lifespan"}
    ) | starter_calls(
        backend / "services/native/target_application_lifecycle.py",
        {"start_target_operational_runtime"},
    )

    assert {
        "start_library_scan_resume_task",
        "start_library_auto_scan_task",
    }.isdisjoint(target)
    assert {
        "start_library_contribution_verification_worker",
        "start_target_scan_supervisor",
        "start_target_identification_worker",
        "start_target_operation_worker",
        "start_target_worker_watchdog",
    } <= target


def test_target_lifecycle_events_sweep_uses_target_catalog_authority() -> None:
    lifecycle = (
        Path(__file__).parents[2] / "services/native/target_application_lifecycle.py"
    )
    module = ast.parse(lifecycle.read_text())
    calls = [
        call
        for call in ast.walk(module)
        if isinstance(call, ast.Call)
        and isinstance(call.func, ast.Name)
        and call.func.id == "start_events_watcher_task"
    ]

    assert len(calls) == 1
    assert isinstance(calls[0].args[0], ast.Name)
    assert calls[0].args[0].id == "get_target_events_watcher_service"


@pytest.mark.parametrize(
    ("admission_token", "expected_phase", "library_enabled"),
    [
        (None, "steady_state", True),
        ("a" * 32, "admission", True),
        (None, "steady_state", False),
    ],
)
def test_production_target_lifespan_selects_validation_phase_and_runs_runtime(
    monkeypatch: pytest.MonkeyPatch,
    admission_token: str | None,
    expected_phase: str,
    library_enabled: bool,
) -> None:
    import target_application as target_module
    from core.dependencies import auth_providers
    from maintenance import automatic_upgrade

    lifecycle_order: list[str] = []
    validate = AsyncMock(side_effect=lambda _phase: lifecycle_order.append("validate"))
    admission = AsyncMock(side_effect=lambda _settings: lifecycle_order.append("admit"))
    init = AsyncMock()
    cleanup = AsyncMock()
    migrate = AsyncMock(side_effect=lambda **_kwargs: lifecycle_order.append("migrate"))
    operational = AsyncMock(
        side_effect=lambda **_kwargs: lifecycle_order.append("operational")
    )
    timezone_name = MagicMock(return_value="Europe/London")
    cache = SimpleNamespace(clear=AsyncMock())
    scan_supervisor_arguments: dict[str, object] = {}
    preferences = SimpleNamespace(
        get_instance_id=lambda: "instance",
        get_advanced_settings=lambda: SimpleNamespace(
            memory_cache_cleanup_interval=60,
            disk_cache_cleanup_interval=60,
        ),
        get_typed_library_settings=lambda: SimpleNamespace(
            library_roots=[], enabled=library_enabled
        ),
        get_library_scan_schedule=lambda: SimpleNamespace(
            scan_frequency="manual", daily_scan_time="03:00"
        ),
    )
    auth = SimpleNamespace(cleanup_expired_tokens=AsyncMock())
    auth_store = object()
    operation_supervisor = SimpleNamespace(
        recover=AsyncMock(
            side_effect=lambda: lifecycle_order.append("operation-recovery")
        )
    )
    recovery_service = SimpleNamespace(
        recover_startup=AsyncMock(
            return_value=SimpleNamespace(
                examined_bundles=0,
                recovered_bundles=0,
                rolled_back_bundles=0,
                needs_attention_bundles=0,
                skipped_bundles=0,
            ),
            side_effect=lambda: (
                lifecycle_order.append("management-recovery")
                or SimpleNamespace(
                    examined_bundles=0,
                    recovered_bundles=0,
                    rolled_back_bundles=0,
                    needs_attention_bundles=0,
                    skipped_bundles=0,
                )
            ),
        )
    )
    monkeypatch.setattr(target_module.TargetStartupValidator, "validate", validate)
    monkeypatch.setattr(automatic_upgrade, "await_target_startup_admission", admission)
    monkeypatch.setattr(target_module, "init_app_state", init)
    monkeypatch.setattr(target_module, "cleanup_app_state", cleanup)
    monkeypatch.setattr(target_module, "run_target_one_time_migrations", migrate)
    monkeypatch.setattr(target_module, "start_target_operational_runtime", operational)
    monkeypatch.setattr(target_module, "_server_timezone_name", timezone_name)
    monkeypatch.setattr(target_module, "get_preferences_service", lambda: preferences)
    work_wakeups = object()
    monkeypatch.setattr(
        target_module,
        "get_native_library_store",
        lambda: SimpleNamespace(work_wakeups=work_wakeups),
    )
    monkeypatch.setattr(target_module, "get_cache", lambda: cache)
    monkeypatch.setattr(target_module, "get_disk_cache", lambda: object())
    monkeypatch.setattr(
        target_module,
        "get_target_library_operation_supervisor",
        lambda: operation_supervisor,
    )
    monkeypatch.setattr(
        target_module,
        "get_library_management_recovery_service",
        lambda: recovery_service,
    )
    monkeypatch.setattr(
        target_module,
        "get_target_consumer_composition",
        lambda: SimpleNamespace(covers=SimpleNamespace(disk_cache=object())),
    )
    monkeypatch.setattr(target_module, "start_cache_cleanup_task", lambda *a, **k: None)
    monkeypatch.setattr(
        target_module, "start_memory_maintenance_task", lambda *a, **k: None
    )
    monkeypatch.setattr(
        target_module, "start_disk_cache_cleanup_task", lambda *a, **k: None
    )
    def _capture_supervisor(*args: object, **kwargs: object) -> object:
        scan_supervisor_arguments["__args"] = args  # type: ignore[assignment]
        scan_supervisor_arguments.update(kwargs)  # type: ignore[arg-type]
        return None

    monkeypatch.setattr(
        target_module,
        "start_target_scan_supervisor",
        _capture_supervisor,
    )
    identification_worker_arguments: dict[str, object] = {}
    monkeypatch.setattr(
        target_module,
        "start_target_identification_worker",
        lambda *a, **k: identification_worker_arguments.update(k),
    )
    operation_worker_arguments: dict[str, object] = {}
    monkeypatch.setattr(
        target_module,
        "start_target_operation_worker",
        lambda *a, **k: operation_worker_arguments.update(k),
    )
    monkeypatch.setattr(
        target_module,
        "start_library_contribution_verification_worker",
        lambda *a, **k: None,
    )
    # (GH-293) The PASSIVE WAL checkpoint task registers a real background loop;
    # drain it here so lifespan tests never leave a pending task behind.
    monkeypatch.setattr(
        target_module, "start_target_wal_checkpoint_task", lambda _service: None
    )
    watchdog_starters: dict[str, object] = {}
    monkeypatch.setattr(
        target_module,
        "start_target_worker_watchdog",
        lambda starters: watchdog_starters.update(starters),
    )
    pending_migration = AsyncMock()
    pending_migration.schedule.return_value = False
    monkeypatch.setattr(
        target_module,
        "get_legacy_pending_migration_service",
        lambda: pending_migration,
    )
    monkeypatch.setattr(auth_providers, "get_auth_service", lambda: auth)
    monkeypatch.setattr(auth_providers, "get_auth_store", lambda: auth_store)
    registry = target_module.TaskRegistry.get_instance()
    shutdown_order: list[str] = []
    monkeypatch.setattr(
        registry,
        "cancel",
        AsyncMock(side_effect=lambda *a, **k: shutdown_order.append("cancel")),
    )
    monkeypatch.setattr(
        registry,
        "cancel_all",
        AsyncMock(side_effect=lambda *a, **k: shutdown_order.append("cancel_all")),
    )
    monkeypatch.setenv("TZ", "Europe/London")
    if admission_token is None:
        monkeypatch.delenv("DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN", raising=False)
    else:
        monkeypatch.setenv("DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN", admission_token)
    app = create_production_target_application()

    with build_test_client(app):
        pass

    validate.assert_awaited_once_with(expected_phase)
    admission.assert_awaited_once()
    migrate.assert_awaited_once_with(
        auth_store=auth_store,
        preferences=preferences,
        cache_dir=target_module.get_settings().cache_dir,
        library_enabled=library_enabled,
    )
    operation_supervisor.recover.assert_awaited_once()
    recovery_service.recover_startup.assert_awaited_once()
    operational.assert_awaited_once_with(
        settings=target_module.get_settings(),
        preferences=preferences,
        auth_store=auth_store,
    )
    identification_enabled_getter = identification_worker_arguments["enabled_getter"]
    operation_enabled_getter = operation_worker_arguments["enabled_getter"]
    assert callable(identification_enabled_getter)
    assert callable(operation_enabled_getter)
    assert identification_enabled_getter() is library_enabled
    assert operation_enabled_getter() is library_enabled
    if library_enabled:
        pending_migration.schedule.assert_awaited_once()
    else:
        pending_migration.schedule.assert_not_awaited()
    schedule_settings_getter = scan_supervisor_arguments["schedule_settings_getter"]
    assert callable(schedule_settings_getter)
    assert schedule_settings_getter()["timezone_name"] == "Europe/London"
    assert schedule_settings_getter()["timezone_name"] == "Europe/London"
    timezone_name.assert_called_once_with()
    supervisor_args = scan_supervisor_arguments.get("__args")  # type: ignore[assignment]
    assert isinstance(supervisor_args, tuple) and len(supervisor_args) == 3
    assert callable(supervisor_args[0])
    assert callable(supervisor_args[1])
    assert supervisor_args[2] is work_wakeups
    assert callable(scan_supervisor_arguments.get("scheduler_getter"))
    assert callable(scan_supervisor_arguments.get("resolver_getter"))
    assert callable(scan_supervisor_arguments.get("schedule_settings_getter"))
    assert set(watchdog_starters) == {
        "target-library-scan-supervisor",
        "target-library-identification-worker",
        "target-library-operation-worker",
        "library-contribution-verification-worker",
    }
    assert all(callable(starter) for starter in watchdog_starters.values())
    registry.cancel.assert_awaited_once_with("target-worker-watchdog")
    assert shutdown_order == ["cancel", "cancel_all"]
    cleanup.assert_awaited_once()
    assert lifecycle_order == [
        "validate",
        "admit",
        "migrate",
        "operation-recovery",
        "management-recovery",
        "operational",
    ]


def test_production_target_lifespan_rejects_malformed_admission_before_validation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import core.config as config_module
    import target_application as target_module
    from maintenance import automatic_upgrade

    validate = AsyncMock()
    monkeypatch.setenv("DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN", "malformed")
    monkeypatch.setattr(config_module, "migrate_legacy_config", lambda: None)
    monkeypatch.setattr(
        target_module,
        "get_settings",
        lambda: SimpleNamespace(log_level="INFO", debug=False, trusted_proxy_ips=[]),
    )
    monkeypatch.setattr(target_module, "init_app_state", AsyncMock())
    monkeypatch.setattr(
        target_module,
        "get_target_library_policy_service",
        lambda: SimpleNamespace(recover_pending_transition=AsyncMock()),
    )
    monkeypatch.setattr(target_module, "get_native_library_store", lambda: object())
    monkeypatch.setattr(
        target_module,
        "get_preferences_service",
        lambda: SimpleNamespace(
            get_typed_library_settings=lambda: SimpleNamespace(library_roots=[])
        ),
    )
    monkeypatch.setattr(target_module.TargetStartupValidator, "validate", validate)
    app = create_production_target_application()

    with pytest.raises(
        automatic_upgrade.AutomaticUpgradeError,
        match="admission token is invalid",
    ):
        with build_test_client(app):
            pass

    validate.assert_not_awaited()
def test_production_target_lifespan_closes_scan_coordinator_on_shutdown(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import target_application as target_module
    from core.dependencies import auth_providers
    from maintenance import automatic_upgrade

    lifecycle_order: list[str] = []
    validate = AsyncMock(side_effect=lambda _phase: lifecycle_order.append("validate"))
    admission = AsyncMock(side_effect=lambda _settings: lifecycle_order.append("admit"))
    init = AsyncMock()
    cleanup = AsyncMock()
    migrate = AsyncMock(side_effect=lambda **_kwargs: lifecycle_order.append("migrate"))
    operational = AsyncMock(side_effect=lambda **_kwargs: lifecycle_order.append("operational"))
    timezone_name = MagicMock(return_value="Europe/London")
    cache = SimpleNamespace(clear=AsyncMock())
    preferences = SimpleNamespace(
        get_instance_id=lambda: "instance",
        get_advanced_settings=lambda: SimpleNamespace(memory_cache_cleanup_interval=60, disk_cache_cleanup_interval=60),
        get_typed_library_settings=lambda: SimpleNamespace(library_roots=[], enabled=True),
        get_library_scan_schedule=lambda: SimpleNamespace(scan_frequency="manual", daily_scan_time="03:00"),
    )
    auth = SimpleNamespace(cleanup_expired_tokens=AsyncMock())
    auth_store = object()
    operation_supervisor = SimpleNamespace(recover=AsyncMock(side_effect=lambda: lifecycle_order.append("operation-recovery")))
    recovery_service = SimpleNamespace(
        recover_startup=AsyncMock(return_value=SimpleNamespace(examined_bundles=0, recovered_bundles=0, rolled_back_bundles=0, needs_attention_bundles=0, skipped_bundles=0), side_effect=lambda: (lifecycle_order.append("management-recovery") or SimpleNamespace(examined_bundles=0, recovered_bundles=0, rolled_back_bundles=0, needs_attention_bundles=0, skipped_bundles=0))),
    )
    monkeypatch.setattr(target_module.TargetStartupValidator, "validate", validate)
    monkeypatch.setattr(automatic_upgrade, "await_target_startup_admission", admission)
    monkeypatch.setattr(target_module, "init_app_state", init)
    monkeypatch.setattr(target_module, "cleanup_app_state", cleanup)
    monkeypatch.setattr(target_module, "run_target_one_time_migrations", migrate)
    monkeypatch.setattr(target_module, "start_target_operational_runtime", operational)
    monkeypatch.setattr(target_module, "_server_timezone_name", timezone_name)
    monkeypatch.setattr(target_module, "get_preferences_service", lambda: preferences)
    monkeypatch.setattr(target_module, "get_native_library_store", lambda: SimpleNamespace(work_wakeups=object()))
    monkeypatch.setattr(target_module, "get_cache", lambda: cache)
    monkeypatch.setattr(target_module, "get_disk_cache", lambda: object())
    monkeypatch.setattr(target_module, "get_target_library_operation_supervisor", lambda: operation_supervisor)
    monkeypatch.setattr(target_module, "get_library_management_recovery_service", lambda: recovery_service)
    monkeypatch.setattr(target_module, "get_target_consumer_composition", lambda: SimpleNamespace(covers=SimpleNamespace(disk_cache=object())))
    monkeypatch.setattr(target_module, "start_cache_cleanup_task", lambda *a, **k: None)
    monkeypatch.setattr(target_module, "start_memory_maintenance_task", lambda *a, **k: None)
    monkeypatch.setattr(target_module, "start_disk_cache_cleanup_task", lambda *a, **k: None)
    monkeypatch.setattr(target_module, "start_target_scan_supervisor", lambda *a, **k: None)
    monkeypatch.setattr(target_module, "start_target_identification_worker", lambda *a, **k: None)
    monkeypatch.setattr(target_module, "start_target_operation_worker", lambda *a, **k: None)
    monkeypatch.setattr(target_module, "start_library_contribution_verification_worker", lambda *a, **k: None)
    monkeypatch.setattr(target_module, "start_target_wal_checkpoint_task", lambda _service: None)
    monkeypatch.setattr(target_module, "start_target_worker_watchdog", lambda starters: None)
    pending_migration = AsyncMock()
    pending_migration.schedule.return_value = False
    monkeypatch.setattr(target_module, "get_legacy_pending_migration_service", lambda: pending_migration)
    monkeypatch.setattr(auth_providers, "get_auth_service", lambda: auth)
    monkeypatch.setattr(auth_providers, "get_auth_store", lambda: auth_store)
    coordinator_close = AsyncMock()
    mock_coordinator = SimpleNamespace(aclose=coordinator_close, close=MagicMock())
    monkeypatch.setattr(target_module, "get_target_library_scan_coordinator", lambda: mock_coordinator)
    registry = target_module.TaskRegistry.get_instance()
    monkeypatch.setattr(registry, "cancel", AsyncMock())
    monkeypatch.setattr(registry, "cancel_all", AsyncMock())
    monkeypatch.setenv("TZ", "Europe/London")
    monkeypatch.delenv("DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN", raising=False)
    app = create_production_target_application()
    with build_test_client(app):
        pass
    coordinator_close.assert_awaited_once()




def test_target_provider_call_graph_has_no_direct_legacy_catalog_edge() -> None:
    backend = Path(__file__).parents[2]
    forbidden = {
        "get_library_repository",
        "get_library_manager",
        "get_library_db",
        "get_file_processor",
        "get_download_orchestrator",
        "get_download_service",
        "get_drop_import_service",
        "get_free_music_service",
        "get_acquisition_dispatcher",
        "get_requests_page_service",
        "get_wanted_watcher_service",
        "get_artist_service",
        "get_artist_discovery_service",
        "get_album_discovery_service",
        "get_home_charts_service",
        "get_coverart_repository",
        "get_genre_index",
        "get_album_release_pin_store",
        "get_navidrome_library_service",
        "get_plex_library_service",
        "get_library_policy_service",
        "get_discover_queue_manager",
    }
    violations: list[tuple[str, str, str]] = []
    sources: dict[str, str] = {}
    for relative in (
        "core/dependencies/service_providers.py",
        "core/dependencies/compat_providers.py",
    ):
        path = backend / relative
        source = path.read_text()
        module = ast.parse(source)
        for node in module.body:
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if not node.name.startswith("get_target_"):
                continue
            sources[node.name] = ast.get_source_segment(source, node) or ""
            for call in (item for item in ast.walk(node) if isinstance(item, ast.Call)):
                if isinstance(call.func, ast.Name) and call.func.id in forbidden:
                    violations.append((relative, node.name, call.func.id))

    assert violations == []
    assert "get_target_import_library_service()" in sources["get_target_file_processor"]
    assert "get_target_file_processor()" in sources["get_target_download_orchestrator"]
    assert (
        "get_target_download_orchestrator()" in sources["get_target_download_service"]
    )
    assert (
        "get_target_drop_import_service()" in sources["get_target_free_music_service"]
    )
    assert (
        "get_target_free_music_service" in sources["get_target_acquisition_dispatcher"]
    )
    assert (
        "get_target_artist_discovery_service()"
        in sources["get_target_discover_service"]
    )
    assert (
        "get_target_album_discovery_service()" in sources["get_target_discover_service"]
    )
    assert "get_target_genre_index()" in sources["get_target_home_charts_service"]
    assert "get_target_genre_index()" in sources["get_target_discover_service"]
    assert "get_target_album_release_pin_store()" in sources["get_target_album_service"]
    assert (
        "get_target_album_release_pin_store()" in sources["get_target_download_service"]
    )
    assert "get_target_coverart_repository()" in sources["get_target_search_service"]
    assert (
        "get_target_library_repository()"
        in sources["get_target_navidrome_library_service"]
    )
    assert (
        "get_target_library_repository()" in sources["get_target_plex_library_service"]
    )


def test_target_cover_provider_has_no_legacy_catalog_inputs(monkeypatch) -> None:
    built = object()
    builder = MagicMock(return_value=built)
    monkeypatch.setattr(repo_providers, "_build_coverart_repository", builder)
    repo_providers.get_target_coverart_repository.cache_clear()

    assert repo_providers.get_target_coverart_repository() is built
    # Mechanical repair during F-PERF-04 verification: this provider now
    # receives the native library store singleton by design (pre-existing
    # stale assertion found failing at HEAD 509e01e before any local edits).
    builder.assert_called_once_with(
        native_library_store=repo_providers.get_native_library_store()
    )

    repo_providers.get_target_coverart_repository.cache_clear()


def test_shared_fingerprinter_reads_only_typed_library_secrets(monkeypatch) -> None:
    preferences = SimpleNamespace(
        get_library_settings=MagicMock(
            side_effect=AssertionError("legacy masked settings read")
        ),
        get_library_settings_raw=MagicMock(
            side_effect=AssertionError("legacy raw settings read")
        ),
        get_typed_library_settings_raw=lambda: SimpleNamespace(
            acoustid_api_key="typed-secret"
        ),
    )
    monkeypatch.setattr(
        service_providers, "get_preferences_service", lambda: preferences
    )
    service_providers.get_audio_fingerprinter.cache_clear()

    fingerprinter = service_providers.get_audio_fingerprinter()

    assert fingerprinter.is_enabled() is True
    preferences.get_library_settings.assert_not_called()
    preferences.get_library_settings_raw.assert_not_called()
    service_providers.get_audio_fingerprinter.cache_clear()


def test_target_album_discovery_provider_returns_target_service(monkeypatch) -> None:
    target = object()
    monkeypatch.setattr(
        service_providers, "get_target_library_repository", lambda: target
    )
    monkeypatch.setattr(
        service_providers, "get_listenbrainz_repository", lambda: object()
    )
    monkeypatch.setattr(
        service_providers, "get_musicbrainz_repository", lambda: object()
    )
    monkeypatch.setattr(service_providers, "get_mbid_store", lambda: object())
    monkeypatch.setattr(
        service_providers, "get_per_user_client_factory", lambda: object()
    )
    monkeypatch.setattr(service_providers, "get_lastfm_repository", lambda: object())
    service_providers.get_target_album_discovery_service.cache_clear()

    try:
        service = service_providers.get_target_album_discovery_service()
    finally:
        service_providers.get_target_album_discovery_service.cache_clear()

    assert isinstance(service, AlbumDiscoveryService)
    assert service._library_repo is target
    assert service._library_db is target


def test_target_provider_route_rejects_local_id_before_metadata_call() -> None:
    ownership = AsyncMock()
    ownership.provider_album_id.side_effect = ProviderIdentityRequiredError(
        "This album only has local metadata."
    )
    metadata = AsyncMock()
    app = create_isolated_target_application(
        dependency_overrides={
            get_target_library_ownership_service: lambda: ownership,
            get_album_service: lambda: metadata,
        }
    )

    response = build_test_client(app).get("/api/v1/albums/local-album")

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "PROVIDER_IDENTITY_REQUIRED"
    metadata.get_album_info.assert_not_awaited()


def test_target_application_refuses_startup_when_validation_fails() -> None:
    async def reject() -> None:
        raise TargetStartupInvariantError("scratch invariant failure")

    app = create_isolated_target_application(startup_validator=reject)

    with pytest.raises(TargetStartupInvariantError, match="scratch invariant failure"):
        with build_test_client(app):
            pass


def test_store_prune_task_receives_the_native_library_singleton() -> None:
    """F-PERF-04: the six-hour store-prune task must receive the native
    library store through the dependency-registry getter - never a separately
    constructed store (AGENTS singleton rule)."""
    lifecycle = (
        Path(__file__).parents[2] / "services/native/target_application_lifecycle.py"
    )
    module = ast.parse(lifecycle.read_text())
    calls = [
        call
        for node in ast.walk(module)
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name == "start_target_operational_runtime"
        for call in ast.walk(node)
        if isinstance(call, ast.Call)
        and isinstance(call.func, ast.Name)
        and call.func.id == "start_store_prune_task"
    ]
    assert len(calls) == 1
    keywords = {keyword.arg: keyword.value for keyword in calls[0].keywords}
    native = keywords.get("native_store")
    assert native is not None, "native_store kwarg missing from start_store_prune_task"
    assert (
        isinstance(native, ast.Call)
        and isinstance(native.func, ast.Name)
        and native.func.id == "get_native_library_store"
    ), "native_store must come from get_native_library_store()"


def _build_production_app_with_base(
    monkeypatch: pytest.MonkeyPatch, base_path: str
):
    """Build the production target app with BASE_PATH forced and settings fresh."""
    monkeypatch.setenv("BASE_PATH", base_path)
    import core.config as config_module

    monkeypatch.setattr(config_module, "_settings", None)
    return build_test_client(create_production_target_application())


def test_production_target_places_base_path_inside_proxy_headers() -> None:
    from api.compat.common.path_case import CompatPathCaseMiddleware
    from core.base_path import BasePathMiddleware
    from uvicorn.middleware.proxy_headers import ProxyHeadersMiddleware

    app = create_production_target_application()
    outermost = [middleware.cls for middleware in app.user_middleware[:3]]
    assert outermost == [
        ProxyHeadersMiddleware,
        BasePathMiddleware,
        CompatPathCaseMiddleware,
    ]


def test_prefixed_surface_serves_health_and_api_under_base(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = _build_production_app_with_base(monkeypatch, "/music")

    prefixed_health = client.get("/music/health")
    assert prefixed_health.status_code == 200
    assert prefixed_health.json() == {
        "status": "ok",
        "message": "DroppedNeedle backend running",
    }

    unprefixed_health = client.get("/health")
    assert unprefixed_health.status_code == 404
    assert unprefixed_health.json() == {
        "error": {"code": "NOT_FOUND", "message": "Not found", "details": None}
    }

    assert client.get("/music/api/v1/openapi.json").status_code == 200
    assert client.get("/api/v1/openapi.json").status_code == 404


def test_prefix_matching_is_segment_exact_and_strips_once(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = _build_production_app_with_base(monkeypatch, "/music")

    for path in ["//music/health", "/music-x/health", "/musicx/health"]:
        response = client.get(path)
        assert response.status_code == 404, path
        assert response.json()["error"]["code"] == "NOT_FOUND", path

    assert client.get("/music/music/health").status_code == 404


def test_auth_middleware_guards_prefixed_api_but_sees_nothing_unprefixed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = _build_production_app_with_base(monkeypatch, "/music")

    protected = client.get("/music/api/v1/artists")
    assert protected.status_code == 401
    assert protected.json()["error"]["code"] == "UNAUTHORIZED"

    hidden = client.get("/api/v1/artists")
    assert hidden.status_code == 404
    assert hidden.json()["error"]["code"] == "NOT_FOUND"


def test_empty_base_path_keeps_today_unprefixed_surface(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = _build_production_app_with_base(monkeypatch, "")

    assert client.get("/health").status_code == 200
    assert client.get("/api/v1/openapi.json").status_code == 200

    protected = client.get("/api/v1/artists")
    assert protected.status_code == 401
    assert protected.json()["error"]["code"] == "UNAUTHORIZED"


_BYPASS_WARNING = (
    "[upgrade] WARNING: library migration launcher was bypassed - expected "
    "`python -m maintenance.automatic_upgrade --start-target`; catalog "
    "migrations did not run. Restore the image CMD."
)


def _bypass_settings(tmp_path: Path, *, root: Path, db_exists: bool) -> SimpleNamespace:
    database = tmp_path / "library.db"
    if db_exists:
        database.write_bytes(b"")
    return SimpleNamespace(
        root_app_dir=root,
        cache_dir=tmp_path,
        library_db_path=database,
    )


def _patch_bypass_environment(
    monkeypatch: pytest.MonkeyPatch,
    automatic_upgrade: object,
    tmp_path: Path,
    *,
    container: bool,
    token: str | None,
) -> None:
    if token is None:
        monkeypatch.delenv("DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN", raising=False)
    else:
        monkeypatch.setenv("DROPPEDNEEDLE_TARGET_ADMISSION_TOKEN", token)
    revision = tmp_path / "source-revision"
    if container:
        revision.write_text("rev")
    monkeypatch.setattr(automatic_upgrade, "_SOURCE_REVISION_PATH", revision)


@pytest.mark.parametrize(
    ("has_marker", "state"),
    [(False, None), (True, {"stage": "promoted_pending_startup"})],
)
def test_launcher_bypass_warns_when_container_db_needs_upgrade(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
    capsys: pytest.CaptureFixture[str],
    has_marker: bool,
    state: dict[str, str] | None,
) -> None:
    import target_application as target_module
    from maintenance import automatic_upgrade

    _patch_bypass_environment(
        monkeypatch, automatic_upgrade, tmp_path, container=True, token=None
    )
    monkeypatch.setattr(
        automatic_upgrade, "_database_has_marker", lambda _db: has_marker
    )
    monkeypatch.setattr(automatic_upgrade, "_read_state", lambda _path: state)
    settings = _bypass_settings(tmp_path, root=Path("/app"), db_exists=True)

    assert target_module._launcher_bypassed(settings) is True
    assert target_module._LAUNCHER_BYPASS_WARNING == _BYPASS_WARNING

    with caplog.at_level(logging.WARNING, logger="target_application"):
        target_module._emit_launcher_bypass_warning()
    assert _BYPASS_WARNING in caplog.text
    assert capsys.readouterr().out.strip() == _BYPASS_WARNING


@pytest.mark.parametrize(
    "state",
    [None, {"stage": "completed"}],
)
def test_launcher_bypass_silent_when_container_db_marked_and_settled(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    state: dict[str, str] | None,
) -> None:
    import target_application as target_module
    from maintenance import automatic_upgrade

    _patch_bypass_environment(
        monkeypatch, automatic_upgrade, tmp_path, container=True, token=None
    )
    monkeypatch.setattr(automatic_upgrade, "_database_has_marker", lambda _db: True)
    monkeypatch.setattr(automatic_upgrade, "_read_state", lambda _path: state)
    settings = _bypass_settings(tmp_path, root=Path("/app"), db_exists=True)

    assert target_module._launcher_bypassed(settings) is False


def test_launcher_bypass_silent_on_fresh_volume(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import target_application as target_module
    from maintenance import automatic_upgrade

    _patch_bypass_environment(
        monkeypatch, automatic_upgrade, tmp_path, container=True, token=None
    )
    marker = MagicMock()
    monkeypatch.setattr(automatic_upgrade, "_database_has_marker", marker)
    settings = _bypass_settings(tmp_path, root=Path("/app"), db_exists=False)

    assert target_module._launcher_bypassed(settings) is False
    marker.assert_not_called()


def test_launcher_bypass_silent_with_admission_token(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import target_application as target_module
    from maintenance import automatic_upgrade

    _patch_bypass_environment(
        monkeypatch, automatic_upgrade, tmp_path, container=True, token="a" * 32
    )
    marker = MagicMock(return_value=False)
    monkeypatch.setattr(automatic_upgrade, "_database_has_marker", marker)
    settings = _bypass_settings(tmp_path, root=Path("/app"), db_exists=True)

    assert target_module._launcher_bypassed(settings) is False
    marker.assert_not_called()


def test_launcher_bypass_silent_on_local_dev_paths(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import target_application as target_module
    from maintenance import automatic_upgrade

    _patch_bypass_environment(
        monkeypatch, automatic_upgrade, tmp_path, container=False, token=None
    )
    marker = MagicMock(return_value=False)
    monkeypatch.setattr(automatic_upgrade, "_database_has_marker", marker)
    settings = _bypass_settings(tmp_path, root=tmp_path, db_exists=True)

    assert target_module._launcher_bypassed(settings) is False
    marker.assert_not_called()


@pytest.mark.asyncio
async def test_launcher_bypass_warning_never_blocks_startup(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    import target_application as target_module

    def _boom(_settings: object) -> bool:
        raise RuntimeError("probe exploded")

    monkeypatch.setattr(target_module, "_launcher_bypassed", _boom)
    settings = _bypass_settings(tmp_path, root=Path("/app"), db_exists=True)

    assert await target_module._warn_if_launcher_bypassed(settings) is False
