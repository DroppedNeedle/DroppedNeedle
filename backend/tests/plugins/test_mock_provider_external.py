"""Third-party loading proof: the shipped example plugin (docs/plugins/example/
mock_provider.py) is copied - byte for byte - into a temporary external plugins
dir, discovered by the manager, and driven through the full acquisition round
trip (configure -> test -> search -> enqueue -> status -> completed_path ->
cancel) plus the protocol adapters the orchestrator would consume."""

import shutil
from pathlib import Path

import pytest

from plugins.base import EnqueueRequest, SearchRequest
from plugins.manager import PluginManager
from repositories.protocols.download_client import DownloadClientProtocol
from repositories.protocols.indexer import IndexerProtocol

_EXAMPLE = (
    Path(__file__).resolve().parents[3] / "docs" / "plugins" / "example" / "mock_provider.py"
)


class _FakePrefs:
    def __init__(self):
        self.store: dict[str, dict] = {}

    def get_plugins_config(self) -> dict:
        return dict(self.store)

    def save_plugin_config_raw(self, plugin_id: str, cfg: dict) -> None:
        self.store[plugin_id] = cfg


@pytest.fixture()
def manager(tmp_path):
    ext = tmp_path / "root" / "plugins"
    ext.mkdir(parents=True)
    shutil.copy2(_EXAMPLE, ext / "mock_provider.py")
    return PluginManager(preferences=_FakePrefs(), external_dir=ext)


def test_example_plugin_is_discovered(manager):
    record = manager.get_record("mock_provider")
    assert record is not None and record.loaded, record.error
    assert record.source == "external"
    assert record.name == "Mock Provider"
    assert manager.is_enabled("mock_provider") is True


@pytest.mark.asyncio
async def test_full_acquisition_round_trip(manager, tmp_path):
    download_dir = tmp_path / "mock-downloads"
    manager.save_settings("mock_provider", {"download_dir": str(download_dir)})
    plugin = manager.get_plugin("mock_provider")

    result = await manager.test("mock_provider")
    assert result.ok, result.message

    candidates = await plugin.search(
        SearchRequest(kind="album", artist_name="Artist", album_title="Album", track_count=10)
    )
    assert len(candidates) == 3
    assert all(c.source == "mock_provider" for c in candidates)
    assert candidates[0].usenet is not None
    assert "Artist - Album" in candidates[0].usenet.title

    handle = await plugin.enqueue(
        EnqueueRequest(task_id="t-42", source="mock_provider", job_name="dn-t-42")
    )
    assert handle.source == "mock_provider"
    assert handle.job_name == "dn-t-42"

    status = await plugin.get_status(handle)
    assert status.status == "completed"
    assert status.files_completed == status.files_total == 2

    files = await plugin.completed_path(handle)
    assert len(files) == 2
    assert all(f.exists() for f in files)
    assert all(str(f).startswith(str(download_dir)) for f in files)

    assert await plugin.cancel(handle) is True
    assert all(not f.exists() for f in files)


@pytest.mark.asyncio
async def test_example_plugin_adapters_satisfy_the_protocols(manager, tmp_path):
    manager.save_settings("mock_provider", {"download_dir": str(tmp_path / "dl")})
    plugin = manager.get_plugin("mock_provider")

    indexer = plugin.get_indexer()
    client = plugin.get_download_client()
    assert isinstance(indexer, IndexerProtocol)
    assert isinstance(client, DownloadClientProtocol)

    results = await indexer.search_album("A", "B")
    assert results and results[0].source == "mock_provider"

    handle = await client.enqueue(
        EnqueueRequest(task_id="t-1", source="mock_provider", job_name="dn-t-1")
    )
    files = await client.list_completed_files(handle)
    assert files and all(f.exists() for f in files)
    assert (await client.get_status(handle)).progress_percent == 100.0
    assert (await client.health_check()).status == "ok"
    assert await client.cancel(handle) is True


def test_secret_field_round_trip(manager):
    from plugins.base import PLUGIN_SECRET_MASK

    manager.save_settings("mock_provider", {"api_token": "tok-123"})
    assert manager.get_settings_values("mock_provider")["api_token"] == PLUGIN_SECRET_MASK
    assert manager.get_settings_values("mock_provider", raw=True)["api_token"] == "tok-123"
