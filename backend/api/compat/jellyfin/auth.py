"""Jellyfin auth resolution (02 s5.2, reference s1)."""

from __future__ import annotations

import re
from typing import TYPE_CHECKING

from fastapi import Request

from core.exceptions import JellyfinError

if TYPE_CHECKING:
    from infrastructure.persistence.auth_store import UserRecord
    from services.compat.app_password_service import AppPasswordService

_TOKEN_RE = re.compile(r'Token="([^"]*)"')
_CLIENT_RE = re.compile(r'Client="([^"]*)"')
_DEVICE_RE = re.compile(r'Device="([^"]*)"')
_DEVICE_ID_RE = re.compile(r'DeviceId="([^"]*)"')


def _mediabrowser_header(request: Request) -> str:
    return (
        request.headers.get("Authorization")
        or request.headers.get("X-Emby-Authorization")
        or ""
    )


def extract_token(request: Request) -> str | None:
    header = _mediabrowser_header(request)
    if header:
        m = _TOKEN_RE.search(header)
        if m and m.group(1):
            return m.group(1)
    for name in ("X-Emby-Token", "X-MediaBrowser-Token"):
        value = request.headers.get(name)
        if value:
            return value
    for q in ("ApiKey", "api_key"):
        value = request.query_params.get(q)
        if value:
            return value
    return None


def extract_client(request: Request) -> str | None:
    m = _CLIENT_RE.search(_mediabrowser_header(request))
    return m.group(1) if m else None


def extract_device(request: Request) -> tuple[str | None, str | None]:
    """(Device, DeviceId) from the MediaBrowser auth header, either None if absent."""
    header = _mediabrowser_header(request)
    name = _DEVICE_RE.search(header)
    device_id = _DEVICE_ID_RE.search(header)
    return (name.group(1) if name else None, device_id.group(1) if device_id else None)


async def resolve_user(
    request: Request, app_passwords: "AppPasswordService"
) -> "UserRecord":
    token = extract_token(request)
    if not token:
        raise JellyfinError(401, "Authentication required")
    user = await app_passwords.verify_token(token)
    if user is None:
        raise JellyfinError(401, "Invalid or expired token")
    return user
