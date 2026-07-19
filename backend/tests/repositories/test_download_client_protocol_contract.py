"""Protocol conformance contract tests (contract-test 3a).

``@runtime_checkable`` ``isinstance`` only verifies method NAMES exist; it does
not check parameter signatures or async-ness. After the search/download split
(D2) there are TWO protocols, each exercised against the real impl + an in-memory
fake:

- ``DownloadClientProtocol`` (acquire/track/locate): ``SlskdRepository`` +
  ``FakeDownloadClient``. ``search_*`` are gone (moved to ``IndexerProtocol``);
  ``list_completed_files`` is new.
- ``IndexerProtocol`` (search): ``SlskdIndexer`` (the adapter) + ``FakeIndexer``.

Each asserts every protocol method's signature (incl. return annotation) and
async-ness match, proving a second client/indexer requires zero
``services/native`` changes.
"""

import inspect
from pathlib import Path

import httpx
import pytest

from repositories.protocols.download_client import DownloadClientProtocol
from repositories.protocols.indexer import IndexerProtocol
from repositories.slskd.slskd_client import SlskdClient
from repositories.slskd.slskd_indexer import SlskdIndexer
from repositories.slskd.slskd_repository import SlskdRepository
from tests.mocks.fake_download_client import FakeDownloadClient, FakeIndexer

_PROTO_METHODS = (
    "is_configured",
    "health_check",
    "enqueue",
    "get_status",
    "cancel",
    "list_completed_files",
    "get_file_path",
    "diagnose_downloads_mount",
)

_INDEXER_METHODS = (
    "is_configured",
    "health_check",
    "search_album",
    "search_track",
)


def _make_slskd_repo() -> SlskdRepository:
    http = httpx.AsyncClient()
    client = SlskdClient(http, "http://slskd", "key")
    return SlskdRepository(
        client=client, url="http://slskd", api_key="key", downloads_mount=Path("/tmp/dl")
    )


def _make_fake_client() -> FakeDownloadClient:
    return FakeDownloadClient()


def _make_sabnzbd_client():
    from repositories.sabnzbd.sabnzbd_client import SabnzbdClient
    from repositories.sabnzbd.sabnzbd_download_client import SabnzbdDownloadClient

    http = httpx.AsyncClient()
    client = SabnzbdClient(http, "http://sab:8080", "key")
    return SabnzbdDownloadClient(client, "http://sab:8080", "key", Path("/sabnzbd-downloads"))


def _make_qbittorrent_client():
    from repositories.qbittorrent.qbittorrent_client import QbittorrentClient
    from repositories.qbittorrent.qbittorrent_download_client import (
        QbittorrentDownloadClient,
    )

    http = httpx.AsyncClient()
    client = QbittorrentClient(http, "http://qbt:8080", "key")
    return QbittorrentDownloadClient(
        client, "http://qbt:8080", "key", Path("/qbittorrent-downloads")
    )


def _make_slskd_indexer() -> SlskdIndexer:
    return SlskdIndexer(_make_slskd_repo())


def _make_fake_indexer() -> FakeIndexer:
    return FakeIndexer()


def _make_newznab_indexer():
    from repositories.newznab.newznab_indexer import NewznabIndexer

    return NewznabIndexer([])  # no configured indexers needed for signature conformance


def _make_prowlarr_indexer():
    from repositories.prowlarr.prowlarr_client import ProwlarrClient
    from repositories.prowlarr.prowlarr_indexer import ProwlarrIndexer

    http = httpx.AsyncClient()
    client = ProwlarrClient(http, "http://prowlarr:9696", "key")
    return ProwlarrIndexer(client, categories=[3000])


@pytest.mark.parametrize(
    "factory",
    [_make_slskd_repo, _make_fake_client, _make_sabnzbd_client, _make_qbittorrent_client],
)
def test_impl_conforms_to_protocol(factory):
    impl = factory()

    # NAME level (what @runtime_checkable / isinstance gives us).
    assert isinstance(impl, DownloadClientProtocol)
    assert isinstance(impl.client_name, str)
    assert impl.client_name
    assert isinstance(impl.supported_sources, frozenset)
    assert impl.supported_sources

    # SIGNATURE + async level (the gap isinstance does NOT cover).
    for name in _PROTO_METHODS:
        proto_fn = getattr(DownloadClientProtocol, name)
        impl_fn = getattr(type(impl), name)
        assert inspect.signature(impl_fn) == inspect.signature(proto_fn), name
        assert inspect.iscoroutinefunction(impl_fn) == inspect.iscoroutinefunction(
            proto_fn
        ), name


@pytest.mark.parametrize(
    "factory",
    [_make_slskd_indexer, _make_fake_indexer, _make_newznab_indexer, _make_prowlarr_indexer],
)
def test_impl_conforms_to_indexer_protocol(factory):
    impl = factory()

    assert isinstance(impl, IndexerProtocol)
    assert isinstance(impl.indexer_name, str)
    assert impl.indexer_name

    for name in _INDEXER_METHODS:
        proto_fn = getattr(IndexerProtocol, name)
        impl_fn = getattr(type(impl), name)
        assert inspect.signature(impl_fn) == inspect.signature(proto_fn), name
        assert inspect.iscoroutinefunction(impl_fn) == inspect.iscoroutinefunction(
            proto_fn
        ), name


def test_registry_routes_torrent_to_second_fake_client(monkeypatch):
    from core.dependencies import repo_providers
    from core.exceptions import ConfigurationError

    fake = FakeDownloadClient()
    monkeypatch.setitem(
        repo_providers._DOWNLOAD_CLIENT_PROVIDERS, "fake-torrent", lambda: fake
    )
    assert (
        repo_providers.get_source_download_client("torrent", "fake-torrent")
        is fake
    )
    with pytest.raises(ConfigurationError, match="does not support"):
        repo_providers.get_source_download_client("usenet", "fake-torrent")
