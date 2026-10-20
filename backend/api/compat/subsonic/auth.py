"""Subsonic auth resolution (02-auth-and-app-passwords.md s5.1)."""

from __future__ import annotations

from typing import TYPE_CHECKING

from core.exceptions import SubsonicError

if TYPE_CHECKING:
    from infrastructure.persistence.auth_store import UserRecord
    from services.compat.app_password_service import AppPasswordService


def _first(params: dict[str, list[str]], key: str) -> str | None:
    vals = params.get(key)
    return vals[0] if vals else None


def presents_secret(params: dict[str, list[str]]) -> bool:
    return any(params.get(key) for key in ("t", "s", "p", "apiKey"))


async def resolve_subsonic_user(
    params: dict[str, list[str]], app_passwords: "AppPasswordService"
) -> "UserRecord":
    """Run the three Subsonic auth schemes; raises SubsonicError(code) on failure."""
    auth_keys = ("u", "t", "s", "p", "apiKey", "c")
    if any(len(params.get(key, [])) > 1 for key in auth_keys):
        raise SubsonicError(10)
    has_token = _first(params, "t") is not None
    has_salt = _first(params, "s") is not None
    has_password = _first(params, "p") is not None
    has_api_key = _first(params, "apiKey") is not None
    if has_token != has_salt:
        raise SubsonicError(10)
    if has_password and has_token:
        raise SubsonicError(10)
    if has_api_key and (has_token or has_password):
        raise SubsonicError(10)
    if not (has_token or has_password or has_api_key):
        raise SubsonicError(10)
    return await app_passwords.verify_subsonic(
        u=_first(params, "u"),
        t=_first(params, "t"),
        s=_first(params, "s"),
        p=_first(params, "p"),
        api_key=_first(params, "apiKey"),
        client=_first(params, "c"),
    )
