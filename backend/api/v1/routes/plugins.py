"""Acquisition-plugin admin routes.

Registry list, enable/disable, per-plugin settings (schema + values, secrets
masked on GET / preserved on masked PUT) and connection test. All admin-gated,
matching the other settings surfaces.
"""

import logging

from fastapi import APIRouter, Depends, HTTPException

from api.v1.schemas.plugins import (
    PluginInfo,
    PluginListResponse,
    PluginSettingsResponse,
    PluginSettingsUpdate,
    PluginTestRequest,
    PluginTestResponse,
    PluginToggleResponse,
)
from core.dependencies import get_plugin_manager
from infrastructure.msgspec_fastapi import MsgSpecBody, MsgSpecRoute
from middleware import CurrentAdminDep
from plugins.base import API_VERSION

logger = logging.getLogger(__name__)

router = APIRouter(route_class=MsgSpecRoute, prefix="/plugins", tags=["plugins"])


def _info(manager, record) -> PluginInfo:
    plugin = record.plugin
    return PluginInfo(
        id=record.id,
        name=record.name or record.id,
        version=record.version,
        builtin=record.builtin,
        enabled=manager.is_enabled(record.id) if record.loaded else False,
        loaded=record.loaded,
        error=record.error,
        source=record.source,
        api_version=getattr(plugin, "api_version", None) if plugin else None,
    )


def _record_or_404(manager, plugin_id: str):
    record = manager.get_record(plugin_id)
    if record is None:
        raise HTTPException(status_code=404, detail=f"Unknown plugin '{plugin_id}'")
    return record


@router.get("", response_model=PluginListResponse)
async def list_plugins(_: CurrentAdminDep, manager=Depends(get_plugin_manager)):
    return PluginListResponse(
        plugins=[_info(manager, r) for r in manager.list_records()],
        api_version=API_VERSION,
    )


@router.post("/{plugin_id}/enable", response_model=PluginToggleResponse)
async def enable_plugin(
    plugin_id: str, _: CurrentAdminDep, manager=Depends(get_plugin_manager)
):
    record = _record_or_404(manager, plugin_id)
    if not record.loaded:
        raise HTTPException(
            status_code=409, detail=record.error or "Plugin failed to load"
        )
    manager.set_enabled(plugin_id, True)
    return PluginToggleResponse(id=plugin_id, enabled=manager.is_enabled(plugin_id))


@router.post("/{plugin_id}/disable", response_model=PluginToggleResponse)
async def disable_plugin(
    plugin_id: str, _: CurrentAdminDep, manager=Depends(get_plugin_manager)
):
    record = _record_or_404(manager, plugin_id)
    if not record.loaded:
        raise HTTPException(
            status_code=409, detail=record.error or "Plugin failed to load"
        )
    manager.set_enabled(plugin_id, False)
    return PluginToggleResponse(id=plugin_id, enabled=manager.is_enabled(plugin_id))


@router.get("/{plugin_id}/settings", response_model=PluginSettingsResponse)
async def get_plugin_settings(
    plugin_id: str, _: CurrentAdminDep, manager=Depends(get_plugin_manager)
):
    record = _record_or_404(manager, plugin_id)
    return PluginSettingsResponse(
        id=plugin_id,
        schema=manager.get_settings_schema(plugin_id),
        values=manager.get_settings_values(record),  # secrets masked
    )


@router.put("/{plugin_id}/settings", response_model=PluginSettingsResponse)
async def update_plugin_settings(
    plugin_id: str,
    _: CurrentAdminDep,
    body: PluginSettingsUpdate = MsgSpecBody(PluginSettingsUpdate),
    manager=Depends(get_plugin_manager),
):
    record = _record_or_404(manager, plugin_id)
    if not record.loaded:
        raise HTTPException(
            status_code=409, detail=record.error or "Plugin failed to load"
        )
    manager.save_settings(plugin_id, body.values)
    return PluginSettingsResponse(
        id=plugin_id,
        schema=manager.get_settings_schema(plugin_id),
        values=manager.get_settings_values(record),  # secrets masked
    )


@router.post("/{plugin_id}/test", response_model=PluginTestResponse)
async def test_plugin(
    plugin_id: str,
    _: CurrentAdminDep,
    body: PluginTestRequest = MsgSpecBody(PluginTestRequest),
    manager=Depends(get_plugin_manager),
):
    """Tests the submitted values (masked secrets resolve to stored ones) without
    saving; an empty body tests the currently saved configuration."""
    _record_or_404(manager, plugin_id)
    result = await manager.test(plugin_id, body.values or None)
    return PluginTestResponse(valid=result.ok, message=result.message, version=result.version)
