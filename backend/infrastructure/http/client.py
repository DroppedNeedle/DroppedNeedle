import threading
from collections import OrderedDict
from collections.abc import Callable
from typing import Any, Optional

import httpx

from core.config import Settings, get_settings
from infrastructure.http.brainzmash_transport import (
    BRAINZMASH_HOST,
    BRAINZMASH_USER_AGENT,
    BrainzMashTransport,
    validate_brainzmash_request_url,
)


def _get_user_agent(settings: Optional[Settings] = None) -> str:
    if settings:
        return settings.get_user_agent()
    return get_settings().get_user_agent()


def _freeze_value(value: Any) -> Any:
    """Deterministic encoding for cache-key participation.

    Hashable values pass through; unhashable containers are canonicalized so
    equal contents always produce an equal key instead of being dropped."""
    if isinstance(value, dict):
        return tuple(sorted((k, _freeze_value(v)) for k, v in value.items()))
    if isinstance(value, (list, tuple)):
        return tuple(_freeze_value(v) for v in value)
    if isinstance(value, (set, frozenset)):
        return frozenset(_freeze_value(v) for v in value)
    try:
        hash(value)
    except TypeError:
        return repr(value)
    return value


class HttpClientFactory:
    """Named outbound HTTP clients with a parameter-aware cache.

    F-PERF-08: the cache key is the immutable effective-construction
    configuration - logical name plus timeout, connect timeout, pool limits,
    HTTP/2, User-Agent identity, and normalized extra kwargs. Equal effective
    configurations for one name share one client; different values never
    silently inherit the first caller's settings. Superseded generations move
    to ``_retired`` on :meth:`retire_name` and are closed by the awaited
    lifecycle paths only - never from a synchronous lookup."""

    _MAX_GENERATIONS = 32

    _clients: "OrderedDict[tuple[int, Any], httpx.AsyncClient]" = OrderedDict()
    _retired: list[httpx.AsyncClient] = []
    _lock = threading.Lock()
    _generation_counter = 0

    @classmethod
    def _effective_key(
        cls,
        *,
        name: str,
        timeout: float,
        connect_timeout: float,
        max_connections: int,
        max_keepalive: int,
        http2: bool,
        follow_redirects: bool = True,
        transport_factory: Callable[[], httpx.AsyncBaseTransport] | None = None,
        user_agent: str,
        kwargs: dict[str, Any],
    ) -> tuple:
        frozen_kwargs = tuple(
            sorted((key, _freeze_value(value)) for key, value in kwargs.items())
        )
        # The leading generation counter keeps logically identical entries from
        # colliding across retirement cycles while remaining fully derived
        # from immutable construction inputs plus the name.
        return (
            hash(
                (
                    name,
                    timeout,
                    connect_timeout,
                    max_connections,
                    max_keepalive,
                    http2,
                    follow_redirects,
                    transport_factory,
                    user_agent,
                    frozen_kwargs,
                )
            ),
            name,
            timeout,
            connect_timeout,
            max_connections,
            max_keepalive,
            http2,
            follow_redirects,
            transport_factory,
            user_agent,
            frozen_kwargs,
        )

    @classmethod
    def get_client(
        cls,
        name: str = "default",
        timeout: float = 10.0,
        connect_timeout: float = 5.0,
        max_connections: int = 200,
        max_keepalive: int = 200,
        settings: Optional[Settings] = None,
        http2: bool = True,
        follow_redirects: bool = True,
        transport_factory: Callable[[], httpx.AsyncBaseTransport] | None = None,
        headers: Optional[dict[str, str]] = None,
        **kwargs,
    ) -> httpx.AsyncClient:
        user_agent = _get_user_agent(settings)
        client_headers = {"User-Agent": user_agent, **(headers or {})}
        key_kwargs = dict(kwargs)
        key_kwargs["headers"] = tuple(sorted(client_headers.items()))
        key = cls._effective_key(
            name=name,
            timeout=timeout,
            connect_timeout=connect_timeout,
            max_connections=max_connections,
            max_keepalive=max_keepalive,
            http2=http2,
            follow_redirects=follow_redirects,
            transport_factory=transport_factory,
            user_agent=user_agent,
            kwargs=key_kwargs,
        )
        with cls._lock:
            existing = cls._clients.get(key)
            if existing is not None:
                cls._clients.move_to_end(key)
                return existing
            # Construction inside the lock coalesces concurrent first access:
            # two callers with one effective key cannot build duplicates.
            client = httpx.AsyncClient(
                http2=http2,
                timeout=httpx.Timeout(timeout, connect=connect_timeout),
                limits=httpx.Limits(
                    max_connections=max_connections,
                    max_keepalive_connections=max_keepalive,
                    keepalive_expiry=60.0,
                ),
                follow_redirects=follow_redirects,
                transport=(
                    transport_factory()
                    if transport_factory is not None
                    else httpx.AsyncHTTPTransport(http2=http2, retries=0)
                ),
                headers=client_headers,
                **kwargs,
            )
            cls._generation_counter += 1
            cls._clients[key] = client
            cls._enforce_generation_cap_locked()
            return client

    @classmethod
    def _enforce_generation_cap_locked(cls) -> None:
        """Bounded history: oldest non-current entries become retired work."""
        while len(cls._clients) > cls._MAX_GENERATIONS:
            _, oldest = cls._clients.popitem(last=False)
            cls._retired.append(oldest)

    @classmethod
    def retire_name(cls, name: str) -> int:
        """Move every active client of a logical name to the retired pool.

        Synchronous and side-effect free beyond bookkeeping: nothing is
        closed here, so an in-flight request holding the previous generation
        is never torn down by a lookup or a settings save."""
        retired_count = 0
        with cls._lock:
            for key in [key for key in cls._clients if key[1] == name]:
                cls._retired.append(cls._clients.pop(key))
                retired_count += 1
        return retired_count

    @classmethod
    async def close_retired(cls) -> int:
        """Awaited close of every superseded generation, exactly once."""
        with cls._lock:
            batch, cls._retired = cls._retired, []
        closed = 0
        for client in batch:
            await client.aclose()
            closed += 1
        return closed

    @classmethod
    async def close_all(cls) -> None:
        """Application shutdown / full reset: close active AND retired
        generations exactly once; safe to call repeatedly."""
        with cls._lock:
            active = list(cls._clients.values())
            cls._clients.clear()
            batch, cls._retired = cls._retired, []
        for client in [*active, *batch]:
            await client.aclose()

    @classmethod
    def reset_for_tests(cls) -> None:
        with cls._lock:
            cls._clients.clear()
            cls._retired.clear()


def get_http_client(
    settings: Optional[Settings] = None,
    timeout: Optional[float] = None,
    connect_timeout: Optional[float] = None,
    max_connections: Optional[int] = None,
) -> httpx.AsyncClient:
    if settings is None:
        settings = get_settings()
    return HttpClientFactory.get_client(
        name="default",
        timeout=timeout or settings.http_timeout,
        connect_timeout=connect_timeout or settings.http_connect_timeout,
        max_connections=max_connections or settings.http_max_connections,
        max_keepalive=settings.http_max_keepalive,
        settings=settings,
    )


async def close_http_clients() -> None:
    await HttpClientFactory.close_all()


def get_listenbrainz_http_client(
    settings: Optional[Settings] = None,
    timeout: Optional[float] = None,
    connect_timeout: Optional[float] = None,
) -> httpx.AsyncClient:
    if settings is None:
        settings = get_settings()
    return HttpClientFactory.get_client(
        name="listenbrainz",
        timeout=timeout or settings.http_timeout,
        connect_timeout=connect_timeout or settings.http_connect_timeout,
        max_connections=20,
        max_keepalive=20,
        settings=settings,
        http2=False,
    )


def get_coverart_http_client(settings: Optional[Settings] = None) -> httpx.AsyncClient:
    """Dedicated client for cover-art fetches (Cover Art Archive -> archive.org CDN,
    Wikidata/Wikimedia, media-server art). Covers are degradable, so this client uses a
    SHORT budget (6s read / 3s connect) rather than the 10s default: a cover that can't be
    had quickly falls through to the placeholder and is warmed in the background instead of
    holding the request open. A separate name is required because HttpClientFactory caches
    by name and the first caller's kwargs win, so the shared "default" client can't be
    retuned for covers without affecting MusicBrainz et al."""
    if settings is None:
        settings = get_settings()
    return HttpClientFactory.get_client(
        name="coverart",
        timeout=6.0,
        connect_timeout=3.0,
        max_connections=settings.http_max_connections,
        max_keepalive=settings.http_max_keepalive,
        settings=settings,
    )


def get_spotify_cover_http_client(
    settings: Optional[Settings] = None,
) -> httpx.AsyncClient:
    """Dedicated client for Spotify CDN playlist-cover fetches (i.scdn.co et al.).
    Covers are optional enrichment, so this uses the same SHORT budget as the
    coverart client (6s read / 3s connect): artwork that can't be had quickly is
    skipped instead of stalling the import. A separate name is required because
    HttpClientFactory caches by name and the first caller's kwargs win."""
    if settings is None:
        settings = get_settings()
    return HttpClientFactory.get_client(
        name="spotify-covers",
        timeout=6.0,
        connect_timeout=3.0,
        max_connections=settings.http_max_connections,
        max_keepalive=settings.http_max_keepalive,
        settings=settings,
    )


async def _sanitize_brainzmash_request(request: httpx.Request) -> None:
    validate_brainzmash_request_url(str(request.url))
    allowed = {
        "accept",
        "accept-encoding",
        "connection",
    }
    headers = {
        key: value
        for key, value in request.headers.items()
        if key.casefold() in allowed
    }
    headers["Host"] = BRAINZMASH_HOST
    headers["User-Agent"] = BRAINZMASH_USER_AGENT
    request.headers = httpx.Headers(headers)


def get_brainzmash_http_client(
    settings: Optional[Settings] = None,
    *,
    timeout: Optional[float] = None,
    connect_timeout: Optional[float] = None,
) -> httpx.AsyncClient:
    """Dedicated BrainzMash client with no redirect or credential forwarding."""
    if settings is None:
        settings = get_settings()
    return HttpClientFactory.get_client(
        name="musicbrainz-brainzmash",
        timeout=timeout or settings.http_timeout,
        connect_timeout=connect_timeout or settings.http_connect_timeout,
        max_connections=1,
        max_keepalive=1,
        settings=settings,
        headers={
            "Accept": "application/json",
            "User-Agent": BRAINZMASH_USER_AGENT,
        },
        http2=False,
        follow_redirects=False,
        transport_factory=BrainzMashTransport,
        event_hooks={"request": [_sanitize_brainzmash_request]},
    )
