"""Orchestrator-via-registry equivalence: the plugin layer must be a lossless
wrapper. The built-in plugins hand the orchestrator the EXACT same singleton
objects the pre-plugin providers built (SlskdRepository / SlskdIndexer /
SabnzbdDownloadClient / NewznabIndexer), so download behaviour is byte-identical
- identity assertions prove it, and would fail on any 'helpful' re-wrapping."""

import pytest

from core.dependencies import (
    get_newznab_indexer,
    get_plugin_manager,
    get_sabnzbd_download_client,
    get_slskd_indexer,
    get_slskd_repository,
)
from core.dependencies._registry import clear_all_singletons


@pytest.fixture(autouse=True)
def _fresh_singletons():
    clear_all_singletons()
    yield
    clear_all_singletons()


def test_builtin_plugins_expose_the_exact_core_singletons():
    manager = get_plugin_manager()

    soulseek = manager.get_plugin("soulseek")
    assert soulseek is not None
    assert soulseek.get_download_client() is get_slskd_repository()
    assert soulseek.get_indexer() is get_slskd_indexer()

    usenet = manager.get_plugin("usenet")
    assert usenet is not None
    assert usenet.get_download_client() is get_sabnzbd_download_client()
    assert usenet.get_indexer() is get_newznab_indexer()


def test_orchestrator_resolves_clients_through_the_registry():
    from core.dependencies.service_providers import get_download_orchestrator

    orchestrator = get_download_orchestrator()
    assert orchestrator._client is get_slskd_repository()
    # the usenet strategy holds the registry-resolved SABnzbd client
    assert orchestrator._strategies["usenet"]._client is get_sabnzbd_download_client()
    assert orchestrator._strategies["soulseek"]._indexer is get_slskd_indexer()
    assert orchestrator._strategies["usenet"]._indexer is get_newznab_indexer()


def test_download_service_resolves_clients_through_the_registry():
    from core.dependencies.service_providers import get_download_service

    service = get_download_service()
    assert service._client is get_slskd_repository()
    assert service._indexer is get_slskd_indexer()
    assert service._usenet_indexer is get_newznab_indexer()


def test_rebuild_after_cache_clear_tracks_fresh_singletons():
    """A settings save cache-clears the slskd chain and rebuilds the orchestrator;
    the plugin must hand the REBUILT singleton to the new orchestrator, never a
    stale captured instance."""
    from core.dependencies.invalidation import clear_soulseek_chain
    from core.dependencies.service_providers import get_download_orchestrator

    first_repo = get_slskd_repository()
    first_orch = get_download_orchestrator()
    assert first_orch._client is first_repo

    clear_soulseek_chain()

    second_repo = get_slskd_repository()
    second_orch = get_download_orchestrator()
    assert second_repo is not first_repo
    assert second_orch is not first_orch
    assert second_orch._client is second_repo


def test_get_download_client_for_source_routes_via_registry():
    from core.dependencies import get_download_client_for_source

    assert get_download_client_for_source("soulseek") is get_slskd_repository()
    assert get_download_client_for_source("usenet") is get_sabnzbd_download_client()
