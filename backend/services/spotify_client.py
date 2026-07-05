"""Spotify Web API client for per-user playlist access."""

from __future__ import annotations

import asyncio
import base64
import logging
from datetime import datetime, timedelta, timezone
from typing import TYPE_CHECKING

import httpx

if TYPE_CHECKING:
    from infrastructure.persistence.user_connections_store import UserConnectionsStore

logger = logging.getLogger(__name__)

_API_BASE = "https://api.spotify.com/v1"
_TOKEN_URL = "https://accounts.spotify.com/api/token"
_REFRESH_BUFFER_SECONDS = 60


def _now_utc() -> datetime:
    return datetime.now(timezone.utc)


class SpotifyAuthError(Exception):
    pass


class SpotifyClient:
    def __init__(
        self,
        client_id: str,
        client_secret: str,
        access_token: str,
        refresh_token: str,
        expires_at: str,
        user_id: str,
        connections_store: UserConnectionsStore,
        spotify_user_id: str = "",
    ) -> None:
        self._client_id = client_id
        self._client_secret = client_secret
        self._access_token = access_token
        self._refresh_token = refresh_token
        self._expires_at = expires_at
        self._user_id = user_id
        self._connections_store = connections_store
        self.spotify_user_id = spotify_user_id

    def _basic_auth_header(self) -> str:
        credentials = f"{self._client_id}:{self._client_secret}"
        return "Basic " + base64.b64encode(credentials.encode()).decode()

    def _is_expired(self) -> bool:
        if not self._expires_at:
            return True
        try:
            expiry = datetime.fromisoformat(self._expires_at)
            if expiry.tzinfo is None:
                expiry = expiry.replace(tzinfo=timezone.utc)
            return _now_utc() >= expiry - timedelta(seconds=_REFRESH_BUFFER_SECONDS)
        except (ValueError, TypeError):
            return True

    async def _refresh(self) -> None:
        if not self._refresh_token:
            raise SpotifyAuthError("No refresh token available")
        async with httpx.AsyncClient() as client:
            resp = await client.post(
                _TOKEN_URL,
                data={"grant_type": "refresh_token", "refresh_token": self._refresh_token},
                headers={"Authorization": self._basic_auth_header()},
                timeout=15,
            )
        if resp.status_code != 200:
            raise SpotifyAuthError(f"Token refresh failed: {resp.status_code}")
        data = resp.json()
        self._access_token = data["access_token"]
        if "refresh_token" in data:
            self._refresh_token = data["refresh_token"]
        self._expires_at = (
            _now_utc() + timedelta(seconds=data.get("expires_in", 3600))
        ).isoformat()
        await self._connections_store.upsert(
            self._user_id,
            "spotify",
            {
                "access_token": self._access_token,
                "refresh_token": self._refresh_token,
                "expires_at": self._expires_at,
            },
        )

    async def _get(self, path: str, params: dict | None = None) -> dict:
        if self._is_expired():
            await self._refresh()
        async with httpx.AsyncClient() as client:
            resp = await client.get(
                f"{_API_BASE}{path}",
                params=params,
                headers={"Authorization": f"Bearer {self._access_token}"},
                timeout=15,
            )
        if resp.status_code == 401:
            await self._refresh()
            async with httpx.AsyncClient() as client:
                resp = await client.get(
                    f"{_API_BASE}{path}",
                    params=params,
                    headers={"Authorization": f"Bearer {self._access_token}"},
                    timeout=15,
                )
        if resp.status_code == 429:
            retry_after = int(resp.headers.get("Retry-After", 2))
            if retry_after <= 10:
                await asyncio.sleep(retry_after)
                async with httpx.AsyncClient() as client:
                    resp = await client.get(
                        f"{_API_BASE}{path}",
                        params=params,
                        headers={"Authorization": f"Bearer {self._access_token}"},
                        timeout=15,
                    )
        resp.raise_for_status()
        return resp.json()

    async def get_current_user(self) -> dict:
        return await self._get("/me")

    async def get_user_playlists(self) -> list[dict]:
        playlists: list[dict] = []
        offset = 0
        limit = 50
        while True:
            page = await self._get(
                "/me/playlists",
                params={"limit": limit, "offset": offset},
            )
            items = page.get("items") or []
            playlists.extend(items)
            if len(playlists) >= page.get("total", 0) or not items:
                break
            offset += limit
        return playlists

    async def get_playlist(self, playlist_id: str) -> dict:
        return await self._get(
            f"/playlists/{playlist_id}",
            params={"fields": "id,name,images,tracks.total"},
        )

    async def get_playlist_tracks(self, playlist_id: str) -> list[dict]:
        tracks: list[dict] = []
        params: dict = {"limit": 100, "offset": 0}
        while True:
            page = await self._get(f"/playlists/{playlist_id}/items", params=params)
            items = page.get("items") or []
            for item in items:
                if item.get("is_local"):
                    continue
                track = item.get("track") or item.get("item")
                if not track or not track.get("id"):
                    continue
                if track.get("type", "track") != "track":
                    continue
                tracks.append(track)
            if not items or not page.get("next"):
                break
            params["offset"] += 100
        return tracks
