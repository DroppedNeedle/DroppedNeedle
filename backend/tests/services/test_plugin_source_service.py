"""PluginSourceService: search proxying, background fetch-and-import handoff,
and the failure notification path."""

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from infrastructure.plugins.protocols import SourceItem
from services.plugin_source_service import PluginSourceService


def _service(tmp_path, host=None):
    if host is None:
        host = MagicMock()
    drop_import = MagicMock()
    drop_import.incoming_dir = lambda: tmp_path / "incoming"
    drop_import.create_job = AsyncMock()
    sse = AsyncMock()
    return (
        PluginSourceService(plugin_host=host, drop_import_service=drop_import, sse_publisher=sse),
        host,
        drop_import,
        sse,
    )


@pytest.mark.asyncio
async def test_search_proxies_to_the_host(tmp_path):
    service, host, _, _ = _service(tmp_path)
    host.source_search = AsyncMock(
        return_value=[SourceItem(id="x", title="Show", artist="Band")]
    )

    items = await service.search("lma-source", "band 1977")

    assert items[0].id == "x"
    host.source_search.assert_awaited_once_with("lma-source", "band 1977")


@pytest.mark.asyncio
async def test_fetch_hands_files_to_the_drop_importer(tmp_path):
    service, host, drop_import, _ = _service(tmp_path)
    host.require_source = MagicMock()

    async def fake_fetch(name: str, item_id: str, dest: Path) -> list[Path]:
        dest.mkdir(parents=True, exist_ok=True)
        f = dest / "t01.flac"
        f.write_bytes(b"x")
        return [f]

    host.source_fetch = AsyncMock(side_effect=fake_fetch)

    service.start_fetch("lma-source", "show-1", user_id="u-1", user_name="Harvey")
    await asyncio.gather(*service._tasks)

    drop_import.create_job.assert_awaited_once()
    kwargs = drop_import.create_job.await_args.kwargs
    assert kwargs["user_id"] == "u-1"
    assert kwargs["uploads"][0][0] == "t01.flac"


@pytest.mark.asyncio
async def test_fetch_failure_notifies_the_user(tmp_path):
    service, host, drop_import, sse = _service(tmp_path)
    host.require_source = MagicMock()
    host.source_fetch = AsyncMock(side_effect=RuntimeError("network down"))

    service.start_fetch("lma-source", "show-1", user_id="u-1", user_name="Harvey")
    await asyncio.gather(*service._tasks)

    drop_import.create_job.assert_not_awaited()
    sse.publish.assert_awaited_once()
    channel, event, payload = sse.publish.await_args.args
    assert channel == "user:u-1"
    assert event == "plugin_fetch_failed"
    assert payload["plugin"] == "lma-source"


@pytest.mark.asyncio
async def test_fetch_with_no_files_notifies_the_user(tmp_path):
    service, host, drop_import, sse = _service(tmp_path)
    host.require_source = MagicMock()
    host.source_fetch = AsyncMock(return_value=[])

    service.start_fetch("lma-source", "show-1", user_id="u-1", user_name="Harvey")
    await asyncio.gather(*service._tasks)

    drop_import.create_job.assert_not_awaited()
    assert sse.publish.await_args.args[1] == "plugin_fetch_failed"
