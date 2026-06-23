from __future__ import annotations

import logging
from collections.abc import AsyncIterator
from typing import Any

import httpx

from core.exceptions import ExternalServiceError, PlexApiError, PlexAuthError
from infrastructure.cache.cache_keys import PLEX_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.resilience.retry import CircuitBreaker, with_retry
from repositories.plex_models import (
    PlexAccount,
    PlexAlbum,
    PlexArtist,
    PlexHistoryEntry,
    PlexLibrarySection,
    PlexOAuthPin,
    PlexPlaylist,
    PlexSession,
    PlexTrack,
    PlexUserProfile,
    StreamProxyResult,
    parse_album,
    parse_artist,
    parse_library_sections,
    parse_plex_history,
    parse_plex_response,
    parse_plex_sessions,
    parse_plex_user_profile,
    parse_plex_users,
    parse_playlist,
    parse_track,
)

logger = logging.getLogger(__name__)

_SOURCE = "plex"

_PLEX_TV_BASE = "https://plex.tv/api/v2"

_PROXY_FORWARD_HEADERS = {"Content-Type", "Content-Length", "Content-Range", "Accept-Ranges"}
_STREAM_CHUNK_SIZE = 64 * 1024

_DEFAULT_TTL_LIST = 300
_DEFAULT_TTL_SEARCH = 120
_DEFAULT_TTL_GENRES = 3600
_DEFAULT_TTL_DETAIL = 300

_plex_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="plex",
)


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))


class PlexRepository:
    def __init__(
        self,
        http_client: httpx.AsyncClient,
        cache: CacheInterface,
    ) -> None:
        self._client = http_client
        self._cache = cache
        self._url: str = ""
        self._token: str = ""
        self._client_id: str = ""
        self._configured: bool = False
        self._ttl_list: int = _DEFAULT_TTL_LIST
        self._ttl_search: int = _DEFAULT_TTL_SEARCH
        self._ttl_genres: int = _DEFAULT_TTL_GENRES
        self._ttl_detail: int = _DEFAULT_TTL_DETAIL
        self._ttl_stats: int = 600

    def configure(self, url: str, token: str, client_id: str = "") -> None:
        self._url = url.rstrip("/") if url else ""
        self._token = token
        self._client_id = client_id
        self._configured = bool(self._url and self._token)

    def is_configured(self) -> bool:
        return self._configured

    @property
    def stats_ttl(self) -> int:
        return self._ttl_stats

    def configure_cache_ttls(
        self,
        *,
        list_ttl: int | None = None,
        search_ttl: int | None = None,
        genres_ttl: int | None = None,
        detail_ttl: int | None = None,
        stats_ttl: int | None = None,
    ) -> None:
        if list_ttl is not None:
            self._ttl_list = list_ttl
        if search_ttl is not None:
            self._ttl_search = search_ttl
        if genres_ttl is not None:
            self._ttl_genres = genres_ttl
        if detail_ttl is not None:
            self._ttl_detail = detail_ttl
        if stats_ttl is not None:
            self._ttl_stats = stats_ttl

    @staticmethod
    def reset_circuit_breaker() -> None:
        _plex_circuit_breaker.reset()

    def _build_headers(self) -> dict[str, str]:
        headers: dict[str, str] = {
            "X-Plex-Token": self._token,
            "X-Plex-Product": "DroppedNeedle",
            "X-Plex-Version": "1.0",
            "Accept": "application/json",
        }
        if self._client_id:
            headers["X-Plex-Client-Identifier"] = self._client_id
        return headers

    def _maybe_upgrade_base_url(
        self, requested_url: str, response: httpx.Response
    ) -> None:
        """Persist an http->https upgrade so we stop paying the redirect each poll.

        Plex behind a TLS-terminating proxy answers http with a 302 to https.
        httpx follows it, but every fresh request restarts from the stored http
        URL and repeats the round-trip. When a redirect only swapped the scheme
        on the same host, rewrite the stored base once so later requests go
        straight to https.
        """
        if not response.history:
            return
        requested = httpx.URL(requested_url)
        final = response.url
        # only a pure scheme swap (same host, port, path) is safe to cache; any
        # other change could corrupt the base (e.g. drop a subpath), so skip it
        if (
            requested.scheme != "http"
            or final.scheme != "https"
            or requested.host != final.host
            or requested.port != final.port
            or requested.path != final.path
        ):
            return
        upgraded = "https://" + self._url.split("://", 1)[1]
        if upgraded != self._url:
            logger.info("Plex base URL auto-upgraded to https: %s", upgraded)
            self._url = upgraded

    @with_retry(
        max_attempts=3,
        base_delay=1.0,
        max_delay=5.0,
        circuit_breaker=_plex_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
        non_breaking_exceptions=(PlexApiError,),
    )
    async def _request(
        self,
        endpoint: str,
        params: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        if not self._configured:
            raise ExternalServiceError("Plex not configured")

        url = f"{self._url}{endpoint}"
        try:
            response = await self._client.get(
                url,
                params=params,
                headers=self._build_headers(),
                timeout=15.0,
            )
        except httpx.TimeoutException as exc:
            raise ExternalServiceError(f"Plex request timed out: {exc}")
        except httpx.HTTPError as exc:
            raise ExternalServiceError(f"Plex request failed: {exc}")

        self._maybe_upgrade_base_url(url, response)

        if response.status_code in (401, 403):
            raise PlexAuthError(
                f"Plex authentication failed ({response.status_code})"
            )
        if response.status_code != 200:
            raise PlexApiError(
                f"Plex request failed ({response.status_code})",
            )

        try:
            data: dict[str, Any] = response.json()
        except Exception as exc:
            raise PlexApiError(f"Plex returned invalid JSON for {endpoint}") from exc
        return parse_plex_response(data)

    async def ping(self) -> bool:
        try:
            if not self._configured:
                return False
            url = f"{self._url}/"
            response = await self._client.get(
                url,
                headers=self._build_headers(),
                timeout=10.0,
            )
            return response.status_code == 200
        except Exception:  # noqa: BLE001
            logger.debug("Plex ping failed", exc_info=True)
            _record_degradation("Plex ping failed")
            return False

    async def get_libraries(self) -> list[PlexLibrarySection]:
        cache_key = f"{PLEX_PREFIX}libraries"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request("/library/sections")
        sections = parse_library_sections(container)
        await self._cache.set(cache_key, sections, self._ttl_list)
        return sections

    async def get_machine_identifier(self) -> str | None:
        cache_key = f"{PLEX_PREFIX}machine_identifier"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached if cached else None

        if not self._configured:
            return None

        try:
            response = await self._client.get(
                f"{self._url}/identity",
                headers = self._build_headers(),
                timeout = 10.0,
            )
        except httpx.HTTPError as exc:
            logger.warning(f"Failed to get Plex machine identifier: {exc}")
            raise ExternalServiceError("Plex request failed") from exc

        if response.status_code != 200:
            return None

        try:
            data = response.json()
        except Exception as exc:
            logger.warning(f"Plex returned invalid JSON for /identity: {exc}")
            raise PlexApiError("Plex returned invalid JSON for /identity") from exc

        machine_id = (
            data.get("MediaContainer", {}).get("machineIdentifier") or data.get("machineIdentifier")
        )
        if machine_id:
            await self._cache.set(cache_key, machine_id, ttl_seconds = 3600)
        else:
            await self._cache.set(cache_key, "", ttl_seconds = 300)
        return machine_id or None

    async def get_music_libraries(self) -> list[PlexLibrarySection]:
        sections = await self.get_libraries()
        return [s for s in sections if s.type == "artist"]

    async def get_artists(
        self,
        section_id: str,
        size: int = 100,
        offset: int = 0,
        search: str = "",
    ) -> list[PlexArtist]:
        cache_key = f"{PLEX_PREFIX}artists:{section_id}:{size}:{offset}:{search}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        params: dict[str, Any] = {
            "type": 8,
            "X-Plex-Container-Start": offset,
            "X-Plex-Container-Size": size,
        }
        if search:
            params["title"] = search
        container = await self._request(
            f"/library/sections/{section_id}/all",
            params=params,
        )
        raw = container.get("Metadata", [])
        artists = [parse_artist(a) for a in raw]
        await self._cache.set(cache_key, artists, self._ttl_list)
        return artists

    async def get_albums(
        self,
        section_id: str,
        size: int = 50,
        offset: int = 0,
        sort: str = "titleSort:asc",
        genre: str | None = None,
        mood: str | None = None,
        decade: str | None = None,
    ) -> tuple[list[PlexAlbum], int]:
        cache_key = f"{PLEX_PREFIX}albums:{section_id}:{size}:{offset}:{sort}:{genre or ''}:{mood or ''}:{decade or ''}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        params: dict[str, Any] = {
            "type": 9,
            "X-Plex-Container-Start": offset,
            "X-Plex-Container-Size": size,
            "sort": sort,
        }
        if genre:
            params["genre"] = genre
        if mood:
            params["mood"] = mood
        if decade:
            stripped = decade.rstrip("s")
            try:
                start = int(stripped)
                params["year"] = ",".join(str(y) for y in range(start, start + 10))
            except ValueError:
                pass

        container = await self._request(
            f"/library/sections/{section_id}/all",
            params=params,
        )
        raw = container.get("Metadata", [])
        total = container.get("totalSize", len(raw))
        albums = [parse_album(a) for a in raw]
        result = (albums, total)
        await self._cache.set(cache_key, result, self._ttl_list)
        return result

    async def get_track_count(self, section_id: str) -> int:
        cache_key = f"{PLEX_PREFIX}track_count:{section_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(
            f"/library/sections/{section_id}/all",
            params={
                "type": 10,
                "X-Plex-Container-Start": 0,
                "X-Plex-Container-Size": 0,
            },
        )
        total = container.get("totalSize", 0)
        await self._cache.set(cache_key, total, self._ttl_list)
        return total

    async def get_artist_count(self, section_id: str) -> int:
        cache_key = f"{PLEX_PREFIX}artist_count:{section_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(
            f"/library/sections/{section_id}/all",
            params={
                "type": 8,
                "X-Plex-Container-Start": 0,
                "X-Plex-Container-Size": 0,
            },
        )
        total = container.get("totalSize", 0)
        await self._cache.set(cache_key, total, self._ttl_list)
        return total

    async def get_tracks(
        self,
        section_id: str,
        size: int = 100,
        offset: int = 0,
        sort: str = "titleSort:asc",
        search: str = "",
        genre: str = "",
    ) -> tuple[list[PlexTrack], int]:
        cache_key = f"{PLEX_PREFIX}tracks:{section_id}:{size}:{offset}:{sort}:{search}:{genre}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        params: dict[str, Any] = {
            "type": 10,
            "sort": sort,
            "X-Plex-Container-Start": offset,
            "X-Plex-Container-Size": size,
        }
        if search:
            params["title"] = search
        if genre:
            params["genre"] = genre
        container = await self._request(
            f"/library/sections/{section_id}/all",
            params=params,
        )
        raw = container.get("Metadata", [])
        tracks = [parse_track(t) for t in raw]
        total = container.get("totalSize", len(tracks))
        result = (tracks, total)
        await self._cache.set(cache_key, result, self._ttl_list)
        return result

    async def get_album_tracks(self, rating_key: str) -> list[PlexTrack]:
        cache_key = f"{PLEX_PREFIX}album_tracks:{rating_key}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(f"/library/metadata/{rating_key}/children")
        raw = container.get("Metadata", [])
        tracks = [parse_track(t) for t in raw]
        await self._cache.set(cache_key, tracks, self._ttl_detail)
        return tracks

    async def get_album_metadata(self, rating_key: str) -> PlexAlbum:
        cache_key = f"{PLEX_PREFIX}album:{rating_key}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(f"/library/metadata/{rating_key}")
        raw = container.get("Metadata", [])
        if not raw:
            raise PlexApiError(f"Album {rating_key} not found")
        album = parse_album(raw[0])
        await self._cache.set(cache_key, album, self._ttl_detail)
        return album

    async def get_recently_added(
        self,
        section_id: str,
        limit: int = 20,
    ) -> list[PlexAlbum]:
        cache_key = f"{PLEX_PREFIX}recent:{section_id}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(
            f"/library/sections/{section_id}/recentlyAdded",
            params={
                "type": 9,
                "X-Plex-Container-Size": limit,
            },
        )
        raw = container.get("Metadata", [])
        albums = [parse_album(a) for a in raw]
        await self._cache.set(cache_key, albums, self._ttl_list)
        return albums

    async def get_recently_viewed(
        self,
        section_id: str,
        limit: int = 20,
    ) -> list[PlexAlbum]:
        cache_key = f"{PLEX_PREFIX}recent_viewed:{section_id}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(
            f"/library/sections/{section_id}/recentlyViewed",
            params={
                "type": 9,
                "X-Plex-Container-Size": limit,
            },
        )
        raw = container.get("Metadata", [])
        albums = [parse_album(a) for a in raw]
        await self._cache.set(cache_key, albums, self._ttl_list)
        return albums

    async def get_playlists(self) -> list[PlexPlaylist]:
        cache_key = f"{PLEX_PREFIX}playlists"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(
            "/playlists",
            params={"playlistType": "audio"},
        )
        raw = container.get("Metadata", [])
        playlists = [parse_playlist(p) for p in raw]
        await self._cache.set(cache_key, playlists, self._ttl_list)
        return playlists

    async def get_playlist_items(self, rating_key: str) -> list[PlexTrack]:
        cache_key = f"{PLEX_PREFIX}playlist:{rating_key}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(f"/playlists/{rating_key}/items")
        raw = container.get("Metadata", [])
        tracks = [parse_track(t) for t in raw]
        await self._cache.set(cache_key, tracks, self._ttl_detail)
        return tracks

    async def search(
        self,
        query: str,
        section_id: str | None = None,
        limit: int = 20,
    ) -> dict[str, list[Any]]:
        cache_key = f"{PLEX_PREFIX}search:{query}:{section_id or ''}:{limit}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        params: dict[str, Any] = {"query": query, "limit": limit}
        if section_id:
            params["sectionId"] = section_id
        container = await self._request("/hubs/search", params=params)

        albums: list[PlexAlbum] = []
        tracks: list[PlexTrack] = []
        artists: list[PlexArtist] = []
        for hub in container.get("Hub", []):
            hub_type = hub.get("type", "")
            for item in hub.get("Metadata", []):
                if hub_type == "album":
                    albums.append(parse_album(item))
                elif hub_type == "track":
                    tracks.append(parse_track(item))
                elif hub_type == "artist":
                    artists.append(parse_artist(item))

        result: dict[str, list[Any]] = {
            "albums": albums,
            "tracks": tracks,
            "artists": artists,
        }
        await self._cache.set(cache_key, result, self._ttl_search)
        return result

    async def get_genres(self, section_id: str) -> list[str]:
        cache_key = f"{PLEX_PREFIX}genres:{section_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(
            f"/library/sections/{section_id}/genre",
        )
        raw = container.get("Directory", [])
        genres = [g.get("title", "") for g in raw if g.get("title")]
        await self._cache.set(cache_key, genres, self._ttl_genres)
        return genres

    async def get_moods(self, section_id: str) -> list[str]:
        cache_key = f"{PLEX_PREFIX}moods:{section_id}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        container = await self._request(
            f"/library/sections/{section_id}/mood",
        )
        raw = container.get("Directory", [])
        moods = [m.get("title", "") for m in raw if m.get("title")]
        await self._cache.set(cache_key, moods, self._ttl_genres)
        return moods

    async def get_hubs(
        self, section_id: str, count: int = 10
    ) -> list[dict[str, Any]]:
        cache_key = f"{PLEX_PREFIX}hubs:{section_id}:{count}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached

        try:
            container = await self._request(
                f"/hubs/sections/{section_id}",
                params={"count": count},
            )
            hubs = container.get("Hub", [])
            await self._cache.set(cache_key, hubs, ttl_seconds=1800)
            return hubs
        except Exception:  # noqa: BLE001
            logger.warning("get_hubs failed for section %s", section_id, exc_info=True)
            _record_degradation("Plex get_hubs failed")
            return []

    async def scrobble(self, rating_key: str) -> bool:
        try:
            if not self._configured:
                return False
            url = f"{self._url}/:/scrobble"
            response = await self._client.get(
                url,
                params={
                    "key": rating_key,
                    "identifier": "com.plexapp.plugins.library",
                },
                headers=self._build_headers(),
                timeout=10.0,
            )
            return response.status_code == 200
        except Exception:  # noqa: BLE001
            logger.warning("Plex scrobble failed for %s", rating_key, exc_info=True)
            _record_degradation("Plex scrobble failed")
            return False

    async def now_playing(self, rating_key: str, state: str = "playing") -> bool:
        try:
            if not self._configured:
                return False
            url = f"{self._url}/:/timeline"
            response = await self._client.get(
                url,
                params={
                    "ratingKey": rating_key,
                    "state": state,
                    "key": f"/library/metadata/{rating_key}",
                },
                headers=self._build_headers(),
                timeout=10.0,
            )
            return response.status_code == 200
        except Exception:  # noqa: BLE001
            logger.warning("Plex now-playing report failed for %s", rating_key, exc_info=True)
            _record_degradation("Plex now-playing report failed")
            return False

    def build_stream_url(self, track: PlexTrack) -> str:
        if not self._configured:
            raise ValueError("Plex is not configured")
        if not track.Media or not track.Media[0].Part:
            raise ValueError(f"Track {track.ratingKey} has no streamable media")
        part_key = track.Media[0].Part[0].key
        return f"{self._url}{part_key}"

    async def proxy_head_stream(self, part_key: str) -> StreamProxyResult:
        if not self._configured:
            raise ExternalServiceError("Plex not configured")

        if not part_key.startswith("/"):
            part_key = f"/{part_key}"

        if ".." in part_key.split("/"):
            raise ValueError(f"Invalid Plex part key: {part_key}")

        if not part_key.startswith("/library/parts/"):
            raise ValueError(f"Invalid Plex part key: {part_key}")

        url = f"{self._url}{part_key}"
        async with httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10, read=10, write=10, pool=10)
        ) as client:
            try:
                resp = await client.head(url, headers=self._build_headers())
            except httpx.HTTPError:
                raise ExternalServiceError("Failed to reach Plex for stream HEAD")

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
        self, part_key: str, range_header: str | None = None
    ) -> StreamProxyResult:
        if not self._configured:
            raise ExternalServiceError("Plex not configured")

        if not part_key.startswith("/"):
            part_key = f"/{part_key}"

        if ".." in part_key.split("/"):
            raise ValueError(f"Invalid Plex part key: {part_key}")

        if not part_key.startswith("/library/parts/"):
            raise ValueError(f"Invalid Plex part key: {part_key}")

        url = f"{self._url}{part_key}"
        upstream_headers = dict(self._build_headers())
        if range_header:
            upstream_headers["Range"] = range_header

        client = httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10, read=120, write=30, pool=10)
        )
        upstream_resp = None
        try:
            upstream_resp = await client.send(
                client.build_request("GET", url, headers=upstream_headers),
                stream=True,
            )

            if upstream_resp.status_code == 416:
                raise ExternalServiceError("416 Range not satisfiable")

            if upstream_resp.status_code >= 400:
                logger.error(
                    "Plex upstream returned %d for %s",
                    upstream_resp.status_code, part_key,
                )
                raise ExternalServiceError("Plex returned an error")

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
        except httpx.HTTPError as exc:
            if upstream_resp:
                await upstream_resp.aclose()
            await client.aclose()
            raise ExternalServiceError(f"Plex streaming failed: {exc}") from exc
        except Exception:
            if upstream_resp:
                await upstream_resp.aclose()
            await client.aclose()
            raise

    async def proxy_thumb(self, rating_key: str, size: int = 500) -> tuple[bytes, str]:
        if not self._configured:
            raise ExternalServiceError("Plex not configured")

        url = f"{self._url}/library/metadata/{rating_key}/thumb"
        try:
            response = await self._client.get(
                url,
                params={"width": size, "height": size},
                headers=self._build_headers(),
                timeout=15.0,
            )
        except httpx.TimeoutException:
            raise ExternalServiceError("Plex thumb request timed out")
        except httpx.HTTPError:
            raise ExternalServiceError("Plex thumb request failed")

        if response.status_code != 200:
            raise ExternalServiceError(
                f"Plex thumb request failed ({response.status_code})"
            )

        content_type = response.headers.get("content-type", "image/jpeg")
        return response.content, content_type

    async def proxy_playlist_composite(self, rating_key: str, size: int = 500) -> tuple[bytes, str]:
        if not self._configured:
            raise ExternalServiceError("Plex not configured")

        playlists = await self.get_playlists()
        playlist = next((p for p in playlists if p.ratingKey == rating_key), None)
        composite_path = playlist.composite if playlist and playlist.composite else f"/playlists/{rating_key}/composite"

        url = f"{self._url}{composite_path}"
        headers = self._build_headers()
        headers["Accept"] = "image/*"
        try:
            response = await self._client.get(
                url,
                params={"width": size, "height": size},
                headers=headers,
                timeout=15.0,
            )
        except httpx.TimeoutException:
            raise ExternalServiceError("Plex playlist composite request timed out")
        except httpx.HTTPError:
            raise ExternalServiceError("Plex playlist composite request failed")

        if response.status_code != 200:
            raise ExternalServiceError(
                f"Plex playlist composite request failed ({response.status_code})"
            )

        content_type = response.headers.get("content-type", "image/jpeg")
        return response.content, content_type

    async def validate_connection(self) -> tuple[bool, str]:
        if not self._configured:
            return False, "Plex URL or token not configured"

        try:
            url = f"{self._url}/"
            response = await self._client.get(
                url,
                headers=self._build_headers(),
                timeout=10.0,
            )
            if response.status_code in (401, 403):
                return False, "Authentication failed - check your Plex token"
            if response.status_code != 200:
                return False, f"Plex returned status {response.status_code}"

            data = response.json()
            container = data.get("MediaContainer", {})
            friendly_name = container.get("friendlyName", "Unknown")
            version = container.get("version", "unknown")
            return True, f"Connected to {friendly_name} (v{version})"
        except httpx.TimeoutException:
            return False, "Connection timed out - check URL"
        except httpx.HTTPError as exc:
            msg = str(exc)
            if "connect" in msg.lower() or "refused" in msg.lower():
                return False, "Could not connect - check URL and ensure server is running"
            return False, f"Connection failed: {msg}"
        except Exception as exc:  # noqa: BLE001
            return False, f"Connection failed: {exc}"

    async def create_oauth_pin(self, client_id: str) -> PlexOAuthPin:
        async with httpx.AsyncClient(timeout=httpx.Timeout(15.0)) as client:
            response = await client.post(
                f"{_PLEX_TV_BASE}/pins",
                headers={
                    "X-Plex-Product": "DroppedNeedle",
                    "X-Plex-Client-Identifier": client_id,
                    "Accept": "application/json",
                },
                data={"strong": "true"},
            )
            if response.status_code != 201:
                raise PlexApiError(f"Failed to create OAuth pin ({response.status_code})")
            data = response.json()
            return PlexOAuthPin(
                id=data.get("id", 0),
                code=data.get("code", ""),
            )

    async def poll_oauth_pin(self, pin_id: int, client_id: str) -> str | None:
        async with httpx.AsyncClient(timeout=httpx.Timeout(10.0)) as client:
            response = await client.get(
                f"{_PLEX_TV_BASE}/pins/{pin_id}",
                headers={
                    "X-Plex-Client-Identifier": client_id,
                    "Accept": "application/json",
                },
            )
            if response.status_code != 200:
                return None
            data = response.json()
            token = data.get("authToken")
            return token if token else None

    async def _plex_tv_get(
        self,
        path: str,
        auth_token: str,
        client_id: str,
        params: dict[str, Any] | None = None,
    ) -> Any:
        # Account-scoped plex.tv call authenticated with a user's auth token (not
        # the configured server token). Used by the login flow.
        headers = {
            "X-Plex-Token": auth_token,
            "X-Plex-Product": "DroppedNeedle",
            "X-Plex-Client-Identifier": client_id,
            "Accept": "application/json",
        }
        try:
            async with httpx.AsyncClient(timeout=httpx.Timeout(15.0)) as client:
                response = await client.get(
                    f"{_PLEX_TV_BASE}{path}", params=params, headers=headers
                )
        except httpx.HTTPError as exc:
            raise PlexApiError(f"Plex request failed: {exc}") from exc
        if response.status_code in (401, 403):
            raise PlexAuthError(f"Plex authentication failed ({response.status_code})")
        if response.status_code != 200:
            raise PlexApiError(f"Plex request failed ({response.status_code})")
        try:
            return response.json()
        except Exception as exc:  # noqa: BLE001
            raise PlexApiError(f"Plex returned invalid JSON for {path}") from exc

    async def get_account_profile(self, auth_token: str, client_id: str) -> PlexUserProfile:
        data = await self._plex_tv_get("/user", auth_token, client_id)
        if not isinstance(data, dict):
            raise PlexApiError("Unexpected Plex /user response shape")
        profile = parse_plex_user_profile(data)
        if not profile.uuid:
            raise PlexApiError("Plex /user response missing uuid")
        return profile

    async def get_account_server_ids(self, auth_token: str, client_id: str) -> set[str]:
        # clientIdentifiers of servers the account can reach (plex.tv /resources).
        # Lenient parse - Plex lists client devices with no accessToken, which the
        # generated SDK model rejected, breaking login for everyone.
        data = await self._plex_tv_get(
            "/resources",
            auth_token,
            client_id,
            params={"includeHttps": 1, "includeRelay": 1},
        )
        devices = data if isinstance(data, list) else []
        server_ids: set[str] = set()
        for device in devices:
            if not isinstance(device, dict):
                continue
            cid = device.get("clientIdentifier")
            provides = device.get("provides") or ""
            if cid and "server" in provides:
                server_ids.add(cid)
        return server_ids

    async def enumerate_users(self) -> list[PlexAccount]:
        # Enumerate Plex Home/managed users + shared friends for admin import
        # (Phase 6, D5). Hits the plex.tv account API with the admin account
        # token, NOT the server. Verified shapes: plex_API_NOTES.md (AMU-7).
        # Home + friends are two calls merged and de-duplicated by uuid.
        if not self._token:
            return []
        headers = self._build_headers()
        home = await self._fetch_plex_tv_accounts(
            f"{_PLEX_TV_BASE}/home/users", "home", headers
        )
        friends = await self._fetch_plex_tv_accounts(
            f"{_PLEX_TV_BASE}/friends", "friend", headers
        )
        merged: dict[str, PlexAccount] = {}
        for account in (*home, *friends):
            merged.setdefault(account.uuid, account)
        return list(merged.values())

    async def _fetch_plex_tv_accounts(
        self, url: str, source: str, headers: dict[str, str]
    ) -> list[PlexAccount]:
        try:
            async with httpx.AsyncClient(timeout=httpx.Timeout(15.0)) as client:
                response = await client.get(url, headers=headers)
            if response.status_code != 200:
                _record_degradation(
                    f"Plex {source} enumeration failed ({response.status_code})"
                )
                return []
            return parse_plex_users(response.json(), source)
        except Exception as exc:  # noqa: BLE001
            _record_degradation(f"Plex {source} enumeration error: {exc}")
            return []

    async def get_sessions(self) -> list[PlexSession]:
        if not self._configured:
            return []
        cache_key = f"{PLEX_PREFIX}sessions"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        try:
            data = await self._request("/status/sessions")
            sessions = parse_plex_sessions(data)
            await self._cache.set(cache_key, sessions, 2)
            return sessions
        except (PlexAuthError, PlexApiError):
            logger.warning("Plex sessions unavailable (may require admin token)")
            _record_degradation("Plex sessions auth/api failure")
            return []
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch Plex sessions", exc_info=True)
            _record_degradation("Plex sessions fetch failed")
            return []

    async def get_listening_history(
        self,
        limit: int = 50,
        offset: int = 0,
    ) -> tuple[list[PlexHistoryEntry], int]:
        if not self._configured:
            return [], 0
        cache_key = f"{PLEX_PREFIX}history:{limit}:{offset}"
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        try:
            data = await self._request(
                "/status/sessions/history/all",
                params={
                    "X-Plex-Container-Start": offset,
                    "X-Plex-Container-Size": limit,
                    "sort": "viewedAt:desc",
                },
            )
            entries, total = parse_plex_history(data)
            result = (entries, total)
            await self._cache.set(cache_key, result, 300)
            return result
        except (PlexAuthError, PlexApiError):
            logger.warning("Plex history unavailable (may require admin token)")
            _record_degradation("Plex history auth/api failure")
            return [], 0
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch Plex listening history", exc_info=True)
            _record_degradation("Plex history fetch failed")
            return [], 0

    async def clear_cache(self) -> None:
        await self._cache.clear_prefix(PLEX_PREFIX)
