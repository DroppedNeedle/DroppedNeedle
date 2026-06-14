import httpx
import logging
import re
from collections.abc import AsyncIterator
from typing import Any
from urllib.parse import urlparse

import msgspec
from core.exceptions import ExternalServiceError, PlaybackNotAllowedError, ResourceNotFoundError
from infrastructure.cache.cache_keys import JELLYFIN_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.persistence import MBIDStore
from infrastructure.constants import BROWSER_AUDIO_DEVICE_PROFILE
from infrastructure.resilience.retry import with_retry, CircuitBreaker
from repositories.jellyfin_models import (
    JellyfinItem,
    JellyfinLyrics,
    JellyfinSession,
    JellyfinUser,
    PlaybackUrlResult,
    parse_item,
    parse_jellyfin_sessions,
    parse_lyrics as parse_jellyfin_lyrics,
    parse_user,
)
from repositories.navidrome_models import StreamProxyResult
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult

logger = logging.getLogger(__name__)

_SOURCE = "jellyfin"


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))

_jellyfin_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="jellyfin"
)

JellyfinJsonObject = dict[str, Any]
JellyfinJsonArray = list[JellyfinJsonObject]
JellyfinJson = JellyfinJsonObject | JellyfinJsonArray


def _decode_json_response(response: httpx.Response) -> JellyfinJson:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=JellyfinJson)
    return response.json()


class JellyfinRepository:
    def __init__(
        self,
        http_client: httpx.AsyncClient,
        cache: CacheInterface,
        base_url: str = "",
        api_key: str = "",
        user_id: str = "",
        mbid_store: MBIDStore | None = None,
    ):
        self._client = http_client
        self._cache = cache
        self._mbid_store = mbid_store
        self._base_url = base_url.rstrip("/") if base_url else ""
        self._api_key = api_key
        self._user_id = user_id
    
    def configure(self, base_url: str, api_key: str, user_id: str = "") -> None:
        self._base_url = base_url.rstrip("/") if base_url else ""
        self._api_key = api_key
        self._user_id = user_id
    
    @staticmethod
    def reset_circuit_breaker() -> None:
        _jellyfin_circuit_breaker.reset()
    
    def is_configured(self) -> bool:
        return bool(self._base_url and self._api_key)
    
    def _get_headers(self) -> dict[str, str]:
        return {
            "Accept": "application/json",
            "Content-Type": "application/json",
            "X-Emby-Token": self._api_key,
        }
    
    @with_retry(
        max_attempts=3,
        base_delay=1.0,
        max_delay=5.0,
        circuit_breaker=_jellyfin_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError)
    )
    async def _request(
        self,
        method: str,
        endpoint: str,
        params: dict[str, Any] | None = None,
        json_data: dict[str, Any] | None = None,
    ) -> Any:
        if not self._base_url or not self._api_key:
            raise ExternalServiceError("Jellyfin not configured")
        
        url = f"{self._base_url}{endpoint}"
        
        try:
            response = await self._client.request(
                method,
                url,
                headers=self._get_headers(),
                params=params,
                json=json_data,
                timeout=15.0,
            )
            
            if response.status_code == 401:
                raise ExternalServiceError("Jellyfin authentication failed - check API key")
            
            if response.status_code == 404:
                return None
            
            if response.status_code not in (200, 204):
                raise ExternalServiceError(
                    f"Jellyfin {method} failed ({response.status_code})",
                    response.text
                )
            
            if response.status_code == 204:
                return None
            
            try:
                return _decode_json_response(response)
            except (msgspec.DecodeError, ValueError, TypeError):
                _record_degradation(f"Jellyfin returned invalid JSON for {method} {endpoint}")
                return None
        
        except httpx.HTTPError as e:
            raise ExternalServiceError(f"Jellyfin request failed: {str(e)}")
    
    async def _get(
        self,
        endpoint: str,
        params: dict[str, Any] | None = None
    ) -> Any:
        return await self._request("GET", endpoint, params=params)
    
    async def validate_connection(self) -> tuple[bool, str]:
        if not self._base_url or not self._api_key:
            return False, "Jellyfin URL or API key not configured"
        
        try:
            url = f"{self._base_url}/System/Info"
            response = await self._client.request(
                "GET",
                url,
                headers=self._get_headers(),
                timeout=10.0,
            )
            
            if response.status_code == 401:
                return False, "Authentication failed - check API key"
            
            if response.status_code != 200:
                return False, f"Connection failed (HTTP {response.status_code})"
            
            result = _decode_json_response(response)
            server_name = result.get("ServerName", "Unknown")
            version = result.get("Version", "Unknown")
            return True, f"Connected to {server_name} (v{version})"
        except httpx.TimeoutException:
            return False, "Connection timed out - check URL"
        except httpx.ConnectError:
            return False, "Could not connect - check URL and ensure server is running"
        except Exception as e:  # noqa: BLE001
            return False, f"Connection failed: {str(e)}"
    
    async def get_users(self) -> list[JellyfinUser]:
        try:
            result = await self._get("/Users")
            if not result:
                return []
            return [parse_user(user) for user in result if user.get("Id")]
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get Jellyfin users: {e}")
            _record_degradation(f"Failed to get users: {e}")
            return []

    async def fetch_users_direct(self) -> list[JellyfinUser]:
        if not self._base_url or not self._api_key:
            return []
        
        try:
            url = f"{self._base_url}/Users"
            response = await self._client.request(
                "GET",
                url,
                headers=self._get_headers(),
                timeout=10.0,
            )
            
            if response.status_code != 200:
                return []
            
            result = _decode_json_response(response)
            if not result:
                return []
            return [parse_user(user) for user in result if user.get("Id")]
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch Jellyfin users: {e}")
            _record_degradation(f"Failed to fetch users: {e}")
            return []

    async def get_current_user(self) -> JellyfinUser | None:
        try:
            result = await self._get("/Users/Me")
            return parse_user(result) if result else None
        except Exception:  # noqa: BLE001
            _record_degradation("Failed to get current user")
            return None

    async def _fetch_items(
        self,
        endpoint: str,
        cache_key: str,
        params: dict[str, Any],
        error_msg: str,
        ttl: int = 300,
        filter_fn=None,
        raise_on_error: bool = False,
    ) -> list[JellyfinItem]:
        cached = await self._cache.get(cache_key)
        if cached:
            return cached
        try:
            result = await self._get(endpoint, params=params)
            if not result:
                if raise_on_error:
                    raise ExternalServiceError(f"{error_msg}: empty response from Jellyfin")
                logger.warning(f"{error_msg}: _get returned None/empty")
                return []
            raw_items = result.get("Items", []) if isinstance(result, dict) else result
            items = [parse_item(i) for i in raw_items if not filter_fn or filter_fn(i)]
            if items:
                await self._cache.set(cache_key, items, ttl_seconds=ttl)
            return items
        except ExternalServiceError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.error(f"{error_msg}: {e}")
            if raise_on_error:
                raise ExternalServiceError(f"{error_msg}: {e}") from e
            _record_degradation(f"{error_msg}: {e}")
            return []

    async def get_recently_played(
        self,
        user_id: str | None = None,
        limit: int = 20,
        ttl_seconds: int = 300,
    ) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        if not uid:
            return []
        params = {"userId": uid, "includeItemTypes": "Audio", "sortBy": "DatePlayed",
                  "sortOrder": "Descending", "isPlayed": "true", "enableUserData": "true",
                  "limit": limit, "recursive": "true", "Fields": "ProviderIds"}
        return await self._fetch_items(
            "/Items",
            f"jellyfin_recent:{uid}:{limit}",
            params,
            "Failed to get recently played",
            ttl=ttl_seconds,
        )

    async def get_favorite_artists(self, user_id: str | None = None, limit: int = 20) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        if not uid:
            return []
        params = {"userId": uid, "isFavorite": "true", "enableUserData": "true", "limit": limit, "Fields": "ProviderIds"}
        return await self._fetch_items("/Artists", f"jellyfin_fav_artists:{uid}:{limit}", params, "Failed to get favorite artists")

    async def get_favorite_albums(
        self,
        user_id: str | None = None,
        limit: int = 20,
        ttl_seconds: int = 300,
    ) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        if not uid:
            return []
        params = {"userId": uid, "includeItemTypes": "MusicAlbum", "isFavorite": "true",
                  "enableUserData": "true", "limit": limit, "recursive": "true"}
        return await self._fetch_items(
            "/Items",
            f"jellyfin_fav_albums:{uid}:{limit}",
            params,
            "Failed to get favorite albums",
            ttl=ttl_seconds,
        )

    async def get_most_played_artists(self, user_id: str | None = None, limit: int = 20) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        if not uid:
            return []
        params = {"userId": uid, "sortBy": "PlayCount", "sortOrder": "Descending",
                  "enableUserData": "true", "limit": limit}
        filter_fn = lambda i: i.get("UserData", {}).get("PlayCount", 0) > 0
        return await self._fetch_items("/Artists", f"jellyfin_top_artists:{uid}:{limit}", params, "Failed to get most played artists", filter_fn=filter_fn)

    async def get_most_played_albums(self, user_id: str | None = None, limit: int = 20) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        if not uid:
            return []
        params = {"userId": uid, "includeItemTypes": "MusicAlbum", "sortBy": "PlayCount",
                  "sortOrder": "Descending", "enableUserData": "true", "limit": limit, "recursive": "true"}
        filter_fn = lambda i: i.get("UserData", {}).get("PlayCount", 0) > 0
        return await self._fetch_items("/Items", f"jellyfin_top_albums:{uid}:{limit}", params, "Failed to get most played albums", filter_fn=filter_fn)

    async def get_recently_added(self, user_id: str | None = None, limit: int = 20) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        if not uid:
            return []
        params = {"userId": uid, "includeItemTypes": "MusicAlbum", "limit": limit, "enableUserData": "true"}
        try:
            result = await self._get("/Items/Latest", params=params)
            return [parse_item(item) for item in result] if result else []
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get recently added: {e}")
            _record_degradation(f"Failed to get recently added: {e}")
            return []

    async def get_genres(self, user_id: str | None = None, ttl_seconds: int = 3600) -> list[str]:
        uid = user_id or self._user_id
        cache_key = f"{JELLYFIN_PREFIX}genres:{uid}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached
        params: dict[str, Any] = {"userId": uid} if uid else {}
        try:
            result = await self._get("/MusicGenres", params=params)
            if not result:
                return []
            genres = [item.get("Name", "") for item in result.get("Items", []) if item.get("Name")]
            await self._cache.set(cache_key, genres, ttl_seconds=ttl_seconds)
            return genres
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get genres: {e}")
            _record_degradation(f"Failed to get genres: {e}")
            return []

    async def get_filter_facets(self, user_id: str | None = None, ttl_seconds: int = 3600) -> dict[str, Any]:
        uid = user_id or self._user_id
        cache_key = f"{JELLYFIN_PREFIX}filter_facets:{uid}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached
        params: dict[str, Any] = {"includeItemTypes": "MusicAlbum"}
        if uid:
            params["userId"] = uid
        try:
            result = await self._get("/Items/Filters", params=params)
            if not result:
                return {"years": [], "tags": [], "studios": []}
            years = sorted(result.get("Years", []), reverse=True)
            tags = sorted(result.get("Tags", []))
            studios = sorted(s for s in result.get("Studios", []) if s)
            facets = {"years": years, "tags": tags, "studios": studios}
            await self._cache.set(cache_key, facets, ttl_seconds=ttl_seconds)
            return facets
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get filter facets: %s", e)
            _record_degradation(f"Failed to get filter facets: {e}")
            return {"years": [], "tags": [], "studios": []}

    async def get_artists_by_genre(self, genre: str, user_id: str | None = None, limit: int = 50) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        params: dict[str, Any] = {"genres": genre, "limit": limit, "enableUserData": "true"}
        if uid:
            params["userId"] = uid
        try:
            result = await self._get("/Artists", params=params)
            return [parse_item(item) for item in result.get("Items", [])] if result else []
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get artists by genre: {e}")
            _record_degradation(f"Failed to get artists by genre: {e}")
            return []
    
    def get_auth_headers(self) -> dict[str, str]:
        return {"X-Emby-Token": self._api_key}

    def get_image_url(self, item_id: str, image_tag: str | None = None) -> str | None:
        if not self._base_url or not item_id:
            return None
        
        url = f"{self._base_url}/Items/{item_id}/Images/Primary"
        if image_tag:
            url += f"?tag={image_tag}"
        
        return url

    async def proxy_image(self, item_id: str, size: int = 500) -> tuple[bytes, str]:
        if not self._base_url or not self._api_key:
            raise ExternalServiceError("Jellyfin not configured")

        url = f"{self._base_url}/Items/{item_id}/Images/Primary"
        params: dict[str, Any] = {
            "maxWidth": size,
            "maxHeight": size,
            "quality": 90,
        }
        try:
            response = await self._client.get(
                url,
                params=params,
                headers={"X-Emby-Token": self._api_key},
                timeout=15.0,
            )
        except httpx.TimeoutException:
            raise ExternalServiceError("Jellyfin image request timed out")
        except httpx.HTTPError:
            raise ExternalServiceError("Jellyfin image request failed")

        if response.status_code != 200:
            raise ExternalServiceError(
                f"Jellyfin image request failed ({response.status_code})"
            )

        content_type = response.headers.get("content-type", "image/jpeg")
        return response.content, content_type

    async def _post(
        self,
        endpoint: str,
        json_data: dict[str, Any] | None = None,
    ) -> Any:
        return await self._request("POST", endpoint, json_data=json_data)

    async def get_albums(
        self,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "SortName",
        sort_order: str = "Ascending",
        genre: str | None = None,
        year: int | None = None,
        tags: str | None = None,
        studios: str | None = None,
    ) -> tuple[list[JellyfinItem], int]:
        uid = self._user_id
        params: dict[str, Any] = {
            "includeItemTypes": "MusicAlbum",
            "recursive": "true",
            "sortBy": sort_by,
            "sortOrder": sort_order,
            "limit": limit,
            "startIndex": offset,
            "enableUserData": "true",
            "Fields": "ProviderIds,ChildCount",
        }
        if uid:
            params["userId"] = uid
        if genre:
            params["genres"] = genre
        if year:
            params["years"] = str(year)
        if tags:
            params["tags"] = tags
        if studios:
            params["studios"] = studios
        cache_key = f"{JELLYFIN_PREFIX}albums:{uid}:{limit}:{offset}:{sort_by}:{sort_order}:{genre}:{year}:{tags}:{studios}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        try:
            result = await self._get("/Items", params=params)
            if not result:
                return [], 0
            raw_items = result.get("Items", [])
            total = result.get("TotalRecordCount", len(raw_items))
            items = [parse_item(i) for i in raw_items]
            pair = (items, total)
            if items:
                await self._cache.set(cache_key, pair, ttl_seconds=120)
            return pair
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get albums: %s", e)
            _record_degradation(f"Failed to get albums: {e}")
            return [], 0

    async def get_album_tracks(self, album_id: str) -> list[JellyfinItem]:
        uid = self._user_id
        params: dict[str, Any] = {
            "albumIds": album_id,
            "includeItemTypes": "Audio",
            "sortBy": "IndexNumber",
            "sortOrder": "Ascending",
            "recursive": "true",
            "enableUserData": "true",
            "Fields": "ProviderIds,MediaStreams",
        }
        if uid:
            params["userId"] = uid
        cache_key = f"{JELLYFIN_PREFIX}album_tracks:{album_id}"
        return await self._fetch_items(
            "/Items",
            cache_key,
            params,
            f"Failed to get tracks for album {album_id}",
            ttl=120,
            raise_on_error=True,
        )

    async def get_album_detail(self, album_id: str) -> JellyfinItem | None:
        uid = self._user_id
        params: dict[str, Any] = {"Fields": "ProviderIds,ChildCount"}
        if uid:
            params["userId"] = uid
        try:
            result = await self._get(f"/Items/{album_id}", params=params)
            return parse_item(result) if result else None
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get album detail {album_id}: {e}")
            _record_degradation(f"Failed to get album detail: {e}")
            return None

    async def get_album_by_mbid(self, musicbrainz_id: str) -> JellyfinItem | None:
        index = await self.build_mbid_index()
        jellyfin_id = index.get(musicbrainz_id)
        if jellyfin_id:
            return await self.get_album_detail(jellyfin_id)

        try:
            results = await self.search_items(musicbrainz_id, item_types="MusicAlbum")
            for item in results:
                if not item.provider_ids:
                    continue
                if (
                    item.provider_ids.get("MusicBrainzReleaseGroup") == musicbrainz_id
                    or item.provider_ids.get("MusicBrainzAlbum") == musicbrainz_id
                ):
                    return item
        except Exception as e:  # noqa: BLE001
            _record_degradation(f"Album MBID search fallback failed: {e}")

        return None

    async def get_artist_by_mbid(self, musicbrainz_id: str) -> JellyfinItem | None:
        try:
            results = await self.search_items(musicbrainz_id, item_types="MusicArtist")
            for item in results:
                if not item.provider_ids:
                    continue
                if item.provider_ids.get("MusicBrainzArtist") == musicbrainz_id:
                    return item
        except Exception as e:  # noqa: BLE001
            _record_degradation(f"Artist MBID search fallback failed: {e}")

        return None

    async def get_artists(
        self, limit: int = 50, offset: int = 0,
        sort_by: str = "SortName", sort_order: str = "Ascending",
        search: str = "",
    ) -> tuple[list[JellyfinItem], int]:
        params: dict[str, Any] = {
            "limit": limit,
            "startIndex": offset,
            "sortBy": sort_by,
            "sortOrder": sort_order,
            "enableUserData": "true",
            "Fields": "ProviderIds",
        }
        if self._user_id:
            params["userId"] = self._user_id
        if search:
            params["searchTerm"] = search
        cache_key = f"{JELLYFIN_PREFIX}artists:{self._user_id}:{limit}:{offset}:{sort_by}:{sort_order}:{search}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        try:
            result = await self._get("/Artists", params=params)
            if not result:
                return [], 0
            raw_items = result.get("Items", [])
            total = result.get("TotalRecordCount", len(raw_items))
            items = [parse_item(i) for i in raw_items]
            pair = (items, total)
            if items:
                await self._cache.set(cache_key, pair, ttl_seconds=120)
            return pair
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get artists: %s", e)
            _record_degradation(f"Failed to get artists: {e}")
            return [], 0

    async def get_tracks(
        self,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "SortName",
        sort_order: str = "Ascending",
        search: str = "",
        genre: str = "",
    ) -> tuple[list[JellyfinItem], int]:
        uid = self._user_id
        params: dict[str, Any] = {
            "includeItemTypes": "Audio",
            "recursive": "true",
            "sortBy": sort_by,
            "sortOrder": sort_order,
            "limit": limit,
            "startIndex": offset,
            "enableUserData": "true",
            "Fields": "ProviderIds",
        }
        if uid:
            params["userId"] = uid
        if search:
            params["searchTerm"] = search
        if genre:
            params["genres"] = genre
        cache_key = f"{JELLYFIN_PREFIX}tracks:{uid}:{limit}:{offset}:{sort_by}:{sort_order}:{search}:{genre}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        try:
            result = await self._get("/Items", params=params)
            if not result:
                return [], 0
            raw_items = result.get("Items", [])
            total = result.get("TotalRecordCount", len(raw_items))
            items = [parse_item(i) for i in raw_items]
            pair = (items, total)
            if items:
                await self._cache.set(cache_key, pair, ttl_seconds=120)
            return pair
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get tracks: %s", e)
            _record_degradation(f"Failed to get tracks: {e}")
            return [], 0

    async def build_mbid_index(self) -> dict[str, str]:
        cache_key = f"{JELLYFIN_PREFIX}mbid_index:{self._user_id or 'default'}"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        if self._mbid_store:
            sqlite_index = await self._mbid_store.load_jellyfin_mbid_index(
                max_age_seconds=3600
            )
            if sqlite_index:
                await self._cache.set(cache_key, sqlite_index, ttl_seconds=3600)
                return sqlite_index

        index: dict[str, str] = {}
        try:
            offset = 0
            batch_size = 500
            while True:
                params: dict[str, Any] = {
                    "includeItemTypes": "MusicAlbum",
                    "recursive": "true",
                    "Fields": "ProviderIds",
                    "limit": batch_size,
                    "startIndex": offset,
                }
                if self._user_id:
                    params["userId"] = self._user_id

                result = await self._get("/Items", params=params)
                if not result:
                    break

                items = result.get("Items", [])
                if not items:
                    break

                for item in items:
                    provider_ids = item.get("ProviderIds", {})
                    item_id = item.get("Id")
                    if not item_id:
                        continue
                    rg_mbid = provider_ids.get("MusicBrainzReleaseGroup")
                    if rg_mbid:
                        index[rg_mbid] = item_id
                    release_mbid = provider_ids.get("MusicBrainzAlbum")
                    if release_mbid:
                        index[release_mbid] = item_id

                total = result.get("TotalRecordCount", 0)
                offset += batch_size
                if offset >= total:
                    break

            if index:
                await self._cache.set(cache_key, index, ttl_seconds=3600)
                if self._mbid_store:
                    await self._mbid_store.save_jellyfin_mbid_index(index)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to build MBID index: {e}")
            _record_degradation(f"Failed to build MBID index: {e}")

        return index

    async def search_items(
        self,
        query: str,
        item_types: str = "MusicAlbum,Audio,MusicArtist",
    ) -> list[JellyfinItem]:
        params: dict[str, Any] = {
            "searchTerm": query,
            "includeItemTypes": item_types,
            "limit": 50,
            "Fields": "ProviderIds",
        }
        if self._user_id:
            params["userId"] = self._user_id
        try:
            result = await self._get("/Search/Hints", params=params)
            if not result:
                return []
            raw_items = result.get("SearchHints", [])
            return [parse_item(item) for item in raw_items]
        except Exception as e:  # noqa: BLE001
            logger.error(f"Jellyfin search failed for '{query}': {e}")
            _record_degradation(f"Search failed: {e}")
            return []

    async def get_library_stats(self, ttl_seconds: int = 600) -> dict[str, Any]:
        cache_key = "jellyfin_library_stats"
        cached = await self._cache.get(cache_key)
        if cached:
            return cached

        stats: dict[str, Any] = {"total_albums": 0, "total_artists": 0, "total_tracks": 0}
        try:
            for item_type, key in [
                ("MusicAlbum", "total_albums"),
                ("MusicArtist", "total_artists"),
                ("Audio", "total_tracks"),
            ]:
                params: dict[str, Any] = {
                    "includeItemTypes": item_type,
                    "recursive": "true",
                    "limit": 0,
                }
                if self._user_id:
                    params["userId"] = self._user_id
                result = await self._get("/Items", params=params)
                if result:
                    stats[key] = result.get("TotalRecordCount", 0)

            await self._cache.set(cache_key, stats, ttl_seconds=ttl_seconds)
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get library stats: {e}")
            _record_degradation(f"Failed to get library stats: {e}")

        return stats

    async def get_playlists(
        self,
        user_id: str | None = None,
        limit: int = 50,
    ) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        cache_key = f"{JELLYFIN_PREFIX}playlists:{uid}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, Any] = {
            "IncludeItemTypes": "Playlist",
            "MediaTypes": "Audio",
            "Recursive": "true",
            "Limit": limit,
            "SortBy": "SortName",
            "SortOrder": "Ascending",
            "Fields": "ChildCount,DateCreated",
        }
        if uid:
            params["UserId"] = uid
        try:
            result = await self._get("/Items", params=params)
            if not result:
                return []
            raw_items = result.get("Items", [])
            items = [parse_item(i) for i in raw_items]
            # Don't cache an empty result: a cold/indexing Jellyfin can briefly
            # return no playlists, and caching that would hide them for the full TTL.
            if items:
                await self._cache.set(cache_key, items, ttl_seconds=300)
            return items
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get Jellyfin playlists: {e}")
            _record_degradation(f"Failed to get playlists: {e}")
            return []

    async def get_playlist(
        self,
        playlist_id: str,
        user_id: str | None = None,
    ) -> JellyfinItem | None:
        uid = user_id or self._user_id
        params: dict[str, Any] = {
            "Fields": "ChildCount,DateCreated,ProviderIds",
        }
        if uid:
            params["UserId"] = uid
        try:
            result = await self._get(f"/Items/{playlist_id}", params=params)
            if not result:
                return None
            return parse_item(result)
        except Exception as e:  # noqa: BLE001
            logger.error("Failed to get Jellyfin playlist %s: %s", playlist_id, e)
            _record_degradation(f"Failed to get playlist detail: {e}")
            return None

    async def get_playlist_items(
        self,
        playlist_id: str,
        user_id: str | None = None,
        limit: int = 1000,
    ) -> list[JellyfinItem]:
        uid = user_id or self._user_id
        cache_key = f"{JELLYFIN_PREFIX}playlist:{playlist_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, Any] = {
            "Limit": limit,
            "Fields": "ProviderIds",
            "EnableUserData": "true",
        }
        if uid:
            params["UserId"] = uid
        try:
            result = await self._get(f"/Playlists/{playlist_id}/Items", params=params)
            if not result:
                return []
            raw_items = result.get("Items", [])
            items = [parse_item(i) for i in raw_items if i.get("Type") == "Audio"]
            await self._cache.set(cache_key, items, ttl_seconds=120)
            return items
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get Jellyfin playlist items for {playlist_id}: {e}")
            _record_degradation(f"Failed to get playlist items: {e}")
            return []

    async def get_instant_mix(
        self,
        item_id: str,
        limit: int = 50,
    ) -> list[JellyfinItem]:
        uid = self._user_id
        cache_key = f"{JELLYFIN_PREFIX}instant_mix:{item_id}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, Any] = {
            "Limit": limit,
            "Fields": "ProviderIds",
            "EnableUserData": "true",
        }
        if uid:
            params["UserId"] = uid
        try:
            result = await self._get(f"/Items/{item_id}/InstantMix", params=params)
            if not result:
                return []
            raw_items = result.get("Items", [])
            items = [parse_item(i) for i in raw_items]
            await self._cache.set(cache_key, items, ttl_seconds=600)
            return items
        except Exception as e:  # noqa: BLE001
            logger.warning(f"Failed to get instant mix for {item_id}: {e}")
            _record_degradation(f"Failed to get instant mix: {e}")
            return []

    async def get_instant_mix_by_artist(
        self,
        artist_id: str,
        limit: int = 50,
    ) -> list[JellyfinItem]:
        uid = self._user_id
        cache_key = f"{JELLYFIN_PREFIX}instant_mix_artist:{artist_id}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, Any] = {
            "Limit": limit,
            "Fields": "ProviderIds",
            "EnableUserData": "true",
        }
        if uid:
            params["UserId"] = uid
        try:
            result = await self._get(f"/Artists/{artist_id}/InstantMix", params=params)
            if not result:
                return []
            raw_items = result.get("Items", [])
            items = [parse_item(i) for i in raw_items]
            await self._cache.set(cache_key, items, ttl_seconds=600)
            return items
        except Exception as e:  # noqa: BLE001
            logger.warning(f"Failed to get artist instant mix for {artist_id}: {e}")
            _record_degradation(f"Failed to get artist instant mix: {e}")
            return []

    async def get_instant_mix_by_genre(
        self,
        genre_name: str,
        limit: int = 50,
    ) -> list[JellyfinItem]:
        uid = self._user_id
        cache_key = f"{JELLYFIN_PREFIX}instant_mix_genre:{genre_name}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, Any] = {
            "Limit": limit,
            "Fields": "ProviderIds",
            "EnableUserData": "true",
        }
        if uid:
            params["UserId"] = uid
        try:
            encoded_genre = genre_name.replace("/", "%2F")
            result = await self._get(f"/MusicGenres/{encoded_genre}/InstantMix", params=params)
            if not result:
                return []
            raw_items = result.get("Items", [])
            items = [parse_item(i) for i in raw_items]
            await self._cache.set(cache_key, items, ttl_seconds=600)
            return items
        except Exception as e:  # noqa: BLE001
            logger.warning(f"Failed to get genre instant mix for {genre_name}: {e}")
            _record_degradation(f"Failed to get genre instant mix: {e}")
            return []

    async def get_similar_items(
        self,
        item_id: str,
        limit: int = 10,
    ) -> list[JellyfinItem]:
        cache_key = f"{JELLYFIN_PREFIX}similar:{item_id}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        params: dict[str, Any] = {
            "Limit": limit,
            "Fields": "ProviderIds",
            "EnableUserData": "true",
        }
        uid = self._user_id
        if uid:
            params["UserId"] = uid
        try:
            result = await self._get(f"/Items/{item_id}/Similar", params=params)
            if not result:
                return []
            raw_items = result.get("Items", [])
            items = [parse_item(i) for i in raw_items]
            await self._cache.set(cache_key, items, ttl_seconds=1800)
            return items
        except Exception as e:  # noqa: BLE001
            logger.warning(f"Failed to get similar items for {item_id}: {e}")
            _record_degradation(f"Failed to get similar items: {e}")
            return []

    async def get_lyrics(self, item_id: str) -> JellyfinLyrics | None:
        cache_key = f"{JELLYFIN_PREFIX}lyrics:{item_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        try:
            result = await self._get(f"/Audio/{item_id}/Lyrics")
            if not result:
                return None
            lyrics = parse_jellyfin_lyrics(result)
            if lyrics:
                await self._cache.set(cache_key, lyrics, 3600)
            return lyrics
        except httpx.HTTPStatusError as exc:
            logger.warning("Jellyfin lyrics HTTP %s for item %s", exc.response.status_code, item_id)
            return None
        except (httpx.HTTPError, msgspec.DecodeError) as exc:
            logger.warning("Jellyfin lyrics fetch/decode error for item %s: %s", item_id, exc)
            return None
        except Exception:  # noqa: BLE001
            logger.warning("Unexpected error fetching lyrics for item %s", item_id, exc_info=True)
            return None

    async def get_sessions(self) -> list[JellyfinSession]:
        if not self.is_configured():
            return []
        uid = self._user_id or "default"
        cache_key = f"{JELLYFIN_PREFIX}sessions:{uid}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        try:
            result = await self._request("GET", "/Sessions")
            if not result or not isinstance(result, list):
                return []
            sessions = parse_jellyfin_sessions(result)
            await self._cache.set(cache_key, sessions, 2)
            return sessions
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch Jellyfin sessions", exc_info=True)
            _record_degradation("Jellyfin sessions fetch failed")
            return []

    async def get_playback_info(self, item_id: str) -> dict[str, Any]:
        params: dict[str, Any] = {}
        if self._user_id:
            params["userId"] = self._user_id
        result = await self._get(f"/Items/{item_id}/PlaybackInfo", params=params)
        if not result:
            raise ResourceNotFoundError(f"Playback info not found for {item_id}")
        return result

    async def get_playback_url(self, item_id: str) -> PlaybackUrlResult:
        params: dict[str, Any] = {}
        if self._user_id:
            params["userId"] = self._user_id

        result = await self._request(
            "POST",
            f"/Items/{item_id}/PlaybackInfo",
            params=params,
            json_data={"DeviceProfile": BROWSER_AUDIO_DEVICE_PROFILE},
        )

        if not result:
            raise ResourceNotFoundError(f"Playback info not found for {item_id}")

        error_code = result.get("ErrorCode")
        if error_code:
            raise PlaybackNotAllowedError(f"Jellyfin playback not allowed: {error_code}")

        raw_play_session_id = result.get("PlaySessionId")
        if not raw_play_session_id:
            play_session_id = ""
        else:
            play_session_id = raw_play_session_id
        media_sources = result.get("MediaSources") or []
        if not media_sources:
            raise ExternalServiceError(f"Playback info missing media sources for {item_id}")

        primary_source = media_sources[0]
        supports_direct_play = bool(primary_source.get("SupportsDirectPlay"))
        supports_direct_stream = bool(primary_source.get("SupportsDirectStream"))
        transcoding_url = primary_source.get("TranscodingUrl")

        if supports_direct_play or supports_direct_stream:
            playback_url = f"{self._base_url}/Audio/{item_id}/stream?static=true"
            play_method = "DirectPlay" if supports_direct_play else "DirectStream"
            seekable = True
        elif isinstance(transcoding_url, str) and transcoding_url:
            playback_url = (
                transcoding_url
                if transcoding_url.startswith(("http://", "https://"))
                else f"{self._base_url}{transcoding_url}"
            )
            play_method = "Transcode"
            seekable = False
        else:
            raise ExternalServiceError(f"Playback info has no playable stream for {item_id}")
        return PlaybackUrlResult(
            url=playback_url,
            seekable=seekable,
            play_session_id=play_session_id,
            play_method=play_method,
        )

    async def report_playback_start(
        self, item_id: str, play_session_id: str, play_method: str = "Transcode"
    ) -> None:
        body: dict[str, Any] = {
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "CanSeek": True,
            "PlayMethod": play_method,
        }
        await self._post("/Sessions/Playing", json_data=body)

    async def report_playback_progress(
        self,
        item_id: str,
        play_session_id: str,
        position_ticks: int,
        is_paused: bool,
    ) -> None:
        body: dict[str, Any] = {
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
            "IsPaused": is_paused,
            "CanSeek": True,
        }
        await self._post("/Sessions/Playing/Progress", json_data=body)

    async def report_playback_stopped(
        self, item_id: str, play_session_id: str, position_ticks: int
    ) -> None:
        body: dict[str, Any] = {
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
        }
        await self._post("/Sessions/Playing/Stopped", json_data=body)

    def _validate_stream_url(self, url: str) -> None:
        expected = urlparse(self._base_url)
        actual = urlparse(url)
        if (actual.scheme, actual.hostname, actual.port) != (
            expected.scheme, expected.hostname, expected.port
        ):
            raise ExternalServiceError(
                "Resolved playback URL does not match configured Jellyfin origin"
            )

    def _get_stream_headers(self) -> dict[str, str]:
        return {"X-Emby-Token": self._api_key}

    async def proxy_head_stream(self, item_id: str) -> StreamProxyResult:
        playback = await self.get_playback_url(item_id)
        self._validate_stream_url(playback.url)

        async with httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10, read=10, write=10, pool=10)
        ) as client:
            try:
                resp = await client.head(
                    playback.url, headers=self._get_stream_headers()
                )
            except httpx.HTTPError:
                raise ExternalServiceError("Failed to reach Jellyfin for stream")

        if resp.status_code >= 400:
            raise ExternalServiceError(
                f"Jellyfin HEAD returned {resp.status_code} for {item_id}"
            )

        headers: dict[str, str] = {}
        for h in _PROXY_FORWARD_HEADERS:
            v = resp.headers.get(h)
            if v:
                headers[h] = v
        return StreamProxyResult(
            status_code=resp.status_code,
            headers=headers,
            media_type=headers.get("Content-Type", "audio/mpeg"),
        )

    async def proxy_get_stream(
        self, item_id: str, range_header: str | None = None
    ) -> StreamProxyResult:
        playback = await self.get_playback_url(item_id)
        self._validate_stream_url(playback.url)

        upstream_headers = self._get_stream_headers()
        if range_header:
            if not _RANGE_RE.match(range_header):
                raise ExternalServiceError("416 Range not satisfiable")
            upstream_headers["Range"] = range_header

        client = httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10, read=120, write=30, pool=10)
        )
        upstream_resp = None
        try:
            try:
                upstream_resp = await client.send(
                    client.build_request("GET", playback.url, headers=upstream_headers),
                    stream=True,
                )
            except httpx.HTTPError as exc:
                raise ExternalServiceError(
                    f"Failed to connect to Jellyfin for stream: {exc}"
                )

            if upstream_resp.status_code == 416:
                raise ExternalServiceError("416 Range not satisfiable")

            if upstream_resp.status_code >= 400:
                logger.error(
                    "Jellyfin upstream returned %d for %s",
                    upstream_resp.status_code, item_id,
                )
                raise ExternalServiceError("Jellyfin returned an error")

            resp_headers: dict[str, str] = {}
            for header_name in _PROXY_FORWARD_HEADERS:
                value = upstream_resp.headers.get(header_name)
                if value:
                    resp_headers[header_name] = value

            status_code = 206 if upstream_resp.status_code == 206 else 200

            async def _stream_body() -> AsyncIterator[bytes]:
                try:
                    async for chunk in upstream_resp.aiter_bytes(
                        chunk_size=_STREAM_CHUNK_SIZE
                    ):
                        yield chunk
                finally:
                    await upstream_resp.aclose()
                    await client.aclose()

            return StreamProxyResult(
                status_code=status_code,
                headers=resp_headers,
                media_type=resp_headers.get("Content-Type", "audio/mpeg"),
                body_chunks=_stream_body(),
            )
        except Exception:  # noqa: BLE001
            if upstream_resp:
                await upstream_resp.aclose()
            await client.aclose()
            raise


_PROXY_FORWARD_HEADERS = {"Content-Type", "Content-Length", "Content-Range", "Accept-Ranges"}
_STREAM_CHUNK_SIZE = 64 * 1024
_RANGE_RE = re.compile(r"^bytes=\d*-\d*(,\s*\d*-\d*)*$")
