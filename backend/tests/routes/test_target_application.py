import ast
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
    policies.get_settings.assert_awaited_once()


def test_deployed_entrypoint_has_no_target_selector_or_target_mount() -> None:
    backend = Path(__file__).parents[2]
    deployed_source = (backend / "main.py").read_text()
    target_source = (backend / "target_application.py").read_text()

    assert "target_application" not in deployed_source
    assert "library_target" not in deployed_source
    assert "get_target_" not in deployed_source
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
        "api.compat.subsonic.router",
        "api.compat.jellyfin.router",
    }.issubset(route_modules)
    assert {
        "AuthMiddleware",
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


def test_target_lifecycle_retains_every_nonlegacy_source_task() -> None:
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

    source = starter_calls(backend / "main.py", {"lifespan"})
    target = starter_calls(
        backend / "target_application.py", {"production_target_lifespan"}
    ) | starter_calls(
        backend / "services/native/target_application_lifecycle.py",
        {"start_target_operational_runtime"},
    )
    replaced = {"start_library_scan_resume_task", "start_library_auto_scan_task"}

    assert source - replaced <= target
    assert replaced.isdisjoint(target)
    assert {
        "start_target_scan_supervisor",
        "start_target_identification_worker",
        "start_target_operation_worker",
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
    ("admission_token", "expected_phase"),
    [(None, "steady_state"), ("a" * 32, "cutover")],
)
def test_production_target_lifespan_selects_validation_phase_and_runs_runtime(
    monkeypatch: pytest.MonkeyPatch,
    admission_token: str | None,
    expected_phase: str,
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
        get_typed_library_settings=lambda: SimpleNamespace(library_roots=[]),
        get_library_scan_schedule=lambda: SimpleNamespace(
            scan_frequency="manual", daily_scan_time="03:00"
        ),
    )
    auth = SimpleNamespace(cleanup_expired_tokens=AsyncMock())
    auth_store = object()
    monkeypatch.setattr(target_module.TargetStartupValidator, "validate", validate)
    monkeypatch.setattr(automatic_upgrade, "await_target_startup_admission", admission)
    monkeypatch.setattr(target_module, "init_app_state", init)
    monkeypatch.setattr(target_module, "cleanup_app_state", cleanup)
    monkeypatch.setattr(target_module, "run_target_one_time_migrations", migrate)
    monkeypatch.setattr(target_module, "start_target_operational_runtime", operational)
    monkeypatch.setattr(target_module, "_server_timezone_name", timezone_name)
    monkeypatch.setattr(target_module, "get_preferences_service", lambda: preferences)
    monkeypatch.setattr(target_module, "get_native_library_store", lambda: object())
    monkeypatch.setattr(target_module, "get_cache", lambda: cache)
    monkeypatch.setattr(target_module, "get_disk_cache", lambda: object())
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
    monkeypatch.setattr(
        target_module,
        "start_target_scan_supervisor",
        lambda *args, **kwargs: scan_supervisor_arguments.update(kwargs),
    )
    monkeypatch.setattr(
        target_module, "start_target_identification_worker", lambda *a, **k: None
    )
    monkeypatch.setattr(
        target_module, "start_target_operation_worker", lambda *a, **k: None
    )
    monkeypatch.setattr(auth_providers, "get_auth_service", lambda: auth)
    monkeypatch.setattr(auth_providers, "get_auth_store", lambda: auth_store)
    monkeypatch.setattr(
        target_module.TaskRegistry.get_instance(), "cancel_all", AsyncMock()
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
    migrate.assert_awaited_once()
    operational.assert_awaited_once_with(
        settings=target_module.get_settings(),
        preferences=preferences,
        auth_store=auth_store,
    )
    schedule_settings_getter = scan_supervisor_arguments["schedule_settings_getter"]
    assert callable(schedule_settings_getter)
    assert schedule_settings_getter()["timezone_name"] == "Europe/London"
    assert schedule_settings_getter()["timezone_name"] == "Europe/London"
    timezone_name.assert_called_once_with()
    cleanup.assert_awaited_once()
    assert lifecycle_order == ["validate", "admit", "migrate", "operational"]


def test_production_target_lifespan_rejects_malformed_admission_after_cutover_validation(
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

    validate.assert_awaited_once_with("cutover")


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
    builder.assert_called_once_with()

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
