"""Tests for the dependencies package structure and registry."""

import importlib

import pytest

from core.dependencies._registry import _singleton_registry, clear_all_singletons


class TestSingletonRegistry:
    def test_registry_is_populated(self):
        # A floor, not an exact count: providers are only ever added, so an exact
        # assertion just churns a magic number each phase. This catches a registry
        # that failed to populate (decorator regression); per-entry correctness is
        # covered by test_all_entries_have_cache_clear below. Phase 4b reached 71.
        assert len(_singleton_registry) >= 71

    def test_all_entries_have_cache_clear(self):
        for fn in _singleton_registry:
            assert hasattr(fn, "cache_clear"), f"{fn.__name__} missing cache_clear"

    def test_clear_all_singletons_calls_cache_clear(self):
        before = [fn.cache_info().currsize for fn in _singleton_registry]
        clear_all_singletons()
        after = [fn.cache_info().currsize for fn in _singleton_registry]
        assert all(s == 0 for s in after)


class TestReExportCompleteness:
    def test_init_exports_all_providers(self):
        init = importlib.import_module("core.dependencies")
        from core.dependencies import cache_providers, repo_providers, service_providers

        for mod in (cache_providers, repo_providers, service_providers):
            for name in dir(mod):
                obj = getattr(mod, name)
                if (
                    name.startswith("get_")
                    and getattr(obj, "__module__", "") == mod.__name__
                ):
                    assert hasattr(init, name), f"{name} not re-exported from __init__"

    def test_init_exports_all_type_aliases(self):
        init = importlib.import_module("core.dependencies")
        from core.dependencies import type_aliases

        for name in dir(type_aliases):
            if name.endswith("Dep"):
                assert hasattr(init, name), f"{name} not re-exported from __init__"

    def test_init_exports_cleanup_functions(self):
        from core.dependencies import (
            init_app_state,
            cleanup_app_state,
            clear_library_management_provider_graph,
            clear_lastfm_dependent_caches,
            clear_listenbrainz_dependent_caches,
            clear_all_singletons,
        )

        assert callable(init_app_state)
        assert callable(cleanup_app_state)
        assert callable(clear_library_management_provider_graph)
        assert callable(clear_lastfm_dependent_caches)
        assert callable(clear_listenbrainz_dependent_caches)
        assert callable(clear_all_singletons)


class TestSingletonDecorator:
    def test_singleton_caches_return_value(self):
        from core.dependencies._registry import singleton

        call_count = 0

        @singleton
        def my_provider():
            nonlocal call_count
            call_count += 1
            return object()

        a = my_provider()
        b = my_provider()
        assert a is b
        assert call_count == 1

        my_provider.cache_clear()
        c = my_provider()
        assert c is not a
        assert call_count == 2

        # clean up: remove from registry
        _singleton_registry.remove(my_provider)


class TestDownloadServiceFreshness:
    """Regression for the stale-scorer bug: the DownloadService singleton is rebuilt on
    a download-policy save, so every long-lived holder must store a GETTER (resolved per
    dispatch) rather than a captured instance - else a saved quality change is silently
    ignored until the app restarts.

    Acquisition now flows through the AcquisitionDispatcher, which itself holds the getter.
    So the freshness guarantee lives in two places: holders that still touch DownloadService
    directly (cancel, scout, purge) keep the getter; dispatch holders keep the dispatcher,
    which keeps the getter. Both are pinned here."""

    def test_direct_download_holders_store_the_getter(self):
        from core.dependencies import service_providers as sp
        from core.dependencies._registry import clear_all_singletons

        try:
            for holder in (
                sp.get_request_service(),  # cancel_task
                sp.get_requests_page_service(),  # cancel_task
                sp.get_discovery_batch_service(),  # purge_album_downloads
                sp.get_wanted_watcher_service(),  # scout, cancel, retry
            ):
                assert holder._get_download_service is sp.get_download_service, (
                    f"{type(holder).__name__} captured a DownloadService instance instead of "
                    "the get_download_service getter"
                )
        finally:
            clear_all_singletons()

    def test_the_dispatcher_holds_the_getter_and_dispatch_holders_hold_the_dispatcher(
        self,
    ):
        from core.dependencies import service_providers as sp
        from core.dependencies._registry import clear_all_singletons

        try:
            dispatcher = sp.get_acquisition_dispatcher()
            assert dispatcher._get_download_service is sp.get_download_service
            assert dispatcher._get_free_music_service is sp.get_free_music_service
            # every acquisition path shares the one dispatcher singleton
            for holder in (
                sp.get_request_service(),
                sp.get_requests_page_service(),
                sp.get_new_release_service(),
                sp.get_personal_mix_service(),
            ):
                assert (
                    holder._acquisition is dispatcher
                ), f"{type(holder).__name__} is not routing through the shared dispatcher"
        finally:
            clear_all_singletons()


class TestCacheProviderConstruction:
    def test_target_cache_service_provider_constructs(self):
        """Regression test for #436: get_target_cache_service must pass every
        CacheService.__init__ arg (it previously omitted mb_response_store,
        raising TypeError on every target-mode cache-service resolution)."""
        from core.dependencies import cache_providers as cp
        from services.native.target_cache_service import TargetCacheService

        try:
            service = cp.get_target_cache_service()
            assert isinstance(service, TargetCacheService)
            assert service._mb_response_store is cp.get_mb_response_store()
        finally:
            clear_all_singletons()
