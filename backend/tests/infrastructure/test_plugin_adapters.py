"""Plugin client inspection must distinguish unavailable evidence from absence."""

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from infrastructure.plugins.adapters import PluginClientAdapter
from repositories.protocols.download_client import DownloadMaterialization, TaskHandle
from services.native.acquisition.errors import OrchestrationError


@pytest.mark.asyncio
async def test_inspection_failure_raises_without_logging_plugin_exception_body(
    caplog: pytest.LogCaptureFixture,
) -> None:
    private_detail = "response body containing secret-token"
    error = RuntimeError(private_detail)
    instance = SimpleNamespace(inspect_materialization=AsyncMock(side_effect=error))
    adapter = PluginClientAdapter("example", instance)
    handle = TaskHandle(source="plugin:example", plugin_token="private-payload")

    with pytest.raises(
        OrchestrationError, match="plugin example materialization inspection failed"
    ) as raised:
        await adapter.inspect_materialization(handle)

    instance.inspect_materialization.assert_awaited_once_with(handle)
    assert raised.value.__cause__ is error
    assert private_detail not in str(raised.value)
    assert private_detail not in caplog.text
    assert handle.plugin_token not in caplog.text
    assert "name=example op=inspect_materialization" in caplog.text


@pytest.mark.asyncio
@pytest.mark.parametrize("state", ["completed", "missing"])
async def test_successful_inspection_preserves_plugin_evidence(state: str) -> None:
    evidence = DownloadMaterialization(state=state)
    instance = SimpleNamespace(inspect_materialization=AsyncMock(return_value=evidence))
    adapter = PluginClientAdapter("example", instance)
    handle = TaskHandle(source="plugin:example")

    assert await adapter.inspect_materialization(handle) is evidence
    instance.inspect_materialization.assert_awaited_once_with(handle)
