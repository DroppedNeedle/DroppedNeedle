import asyncio
import math
import random
import threading
import time
import uuid
from contextvars import ContextVar
from dataclasses import dataclass
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from typing import Any, Awaitable, Callable, TypeVar
from urllib.parse import urlsplit

import httpx
import msgspec
from core.exceptions import (
    ConfigurationError,
    ExternalServiceError,
    InvalidExternalPayloadError,
    NonRetriableExternalServiceError,
    RateLimitedError,
)
from infrastructure.resilience import retry as retry_module
from infrastructure.resilience.retry import with_retry, CircuitBreaker
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.queue.priority_queue import RequestPriority, get_priority_queue
from infrastructure.http.deduplication import RequestDeduplicator
from infrastructure.service_health import report_breaker_health
from infrastructure.observability.provider_counters import (
    record_provider_call,
    record_rate_limit_headers,
)
from infrastructure.http.brainzmash_transport import (
    BRAINZMASH_ENDPOINT,
    validate_brainzmash_path,
    validate_brainzmash_url,
)
from repositories.edition_policy import recall_key

T = TypeVar("T")

OFFICIAL_MB_API_BASE = "https://musicbrainz.org/ws/2"
_mb_api_base: str = OFFICIAL_MB_API_BASE
_mb_source_generation = 0
_mb_source_mode = "official"
_mb_source_id = ""
_brainzmash_runtime_enabled = False
_mb_operation_context: ContextVar["MbSourceContext | None"] = ContextVar(
    "musicbrainz_operation_context", default=None
)


@dataclass(frozen=True)
class MbSourceContext:
    source_url: str
    generation: int
    source_mode: str = "official"
    source_id: str = ""


_mb_response_context: ContextVar[MbSourceContext | None] = ContextVar(
    "musicbrainz_response_context", default=None
)


class _ProcessWideAsyncLock:
    """Loop-agnostic async facade over one process-wide mutex."""

    def __init__(self) -> None:
        self._lock = threading.Lock()

    def locked(self) -> bool:
        return self._lock.locked()

    async def acquire(self) -> bool:
        while not self._lock.acquire(blocking=False):
            await asyncio.sleep(0.001)
        return True

    def release(self) -> None:
        self._lock.release()

    async def __aenter__(self):
        await self.acquire()
        return self

    async def __aexit__(self, exc_type, exc, tb) -> None:
        self.release()


mb_source_commit_lock = _ProcessWideAsyncLock()


def clear_mb_response_context() -> None:
    """Drop any prior wire context before a cache/durable-only path."""
    _mb_response_context.set(None)


def _stale_source_error() -> ConfigurationError:
    clear_mb_response_context()
    return ConfigurationError("MusicBrainz source changed during the request")


def normalize_mb_source_label(url: str | None) -> str:
    """Return a privacy-safe source origin without credentials or URL detail."""
    if not isinstance(url, str) or not url.strip():
        return ""
    try:
        parsed = urlsplit(url.strip())
        hostname = parsed.hostname
        if parsed.scheme.lower() not in {"http", "https"} or not hostname:
            return ""
        port = parsed.port
    except (AttributeError, TypeError, ValueError):
        return ""

    host = hostname.casefold()
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    return f"{parsed.scheme.lower()}://{host}{f':{port}' if port is not None else ''}"


_MB_RATE_POLICY_PUBLIC_ORIGINS = frozenset(
    {
        "http://musicbrainz.org",
        "http://musicbrainz.org:80",
        "http://www.musicbrainz.org",
        "http://www.musicbrainz.org:80",
        "https://musicbrainz.org",
        "https://musicbrainz.org:443",
        "https://www.musicbrainz.org",
        "https://www.musicbrainz.org:443",
    }
)
MB_TRUSTED_IDENTITY_ORIGINS: tuple[str, ...] = (
    "https://musicbrainz.org",
    "https://musicbrainz.org:443",
    "https://www.musicbrainz.org",
    "https://www.musicbrainz.org:443",
)


def is_mb_rate_policy_public_host(url: str | None) -> bool:
    """Classify public MusicBrainz origins for transport-rate policy only.

    This intentionally includes HTTP so choosing an insecure transport cannot
    bypass the official request ceiling. It must not be used as identity or
    durable-provenance proof.
    """
    return normalize_mb_source_label(url) in _MB_RATE_POLICY_PUBLIC_ORIGINS


def is_mb_identity_source(url: str | None) -> bool:
    """Accept only TLS/default-port MusicBrainz origins as identity proof."""
    return normalize_mb_source_label(url) in MB_TRUSTED_IDENTITY_ORIGINS


def get_mb_api_base() -> str:
    return _mb_api_base


def get_mb_source_generation() -> int:
    return _mb_source_generation


def get_mb_source_mode() -> str:
    return _mb_source_mode


def get_mb_source_id() -> str:
    return _mb_source_id


def capture_mb_source_context() -> MbSourceContext:
    """Capture the source context before a provider service operation."""
    context = MbSourceContext(
        source_url=_mb_api_base,
        generation=_mb_source_generation,
        source_mode=_mb_source_mode,
        source_id=_mb_source_id,
    )
    _mb_operation_context.set(context)
    return context


def set_brainzmash_runtime_enabled(enabled: bool) -> None:
    global _brainzmash_runtime_enabled
    _brainzmash_runtime_enabled = bool(enabled)


def brainzmash_runtime_enabled() -> bool:
    return _brainzmash_runtime_enabled


def get_mb_operation_context() -> MbSourceContext | None:
    return _mb_operation_context.get()


def mb_cache_namespace(context: MbSourceContext | None = None) -> str:
    context = context or _mb_operation_context.get()
    if context is None:
        return ""
    source_id = context.source_id or "legacy"
    return f"source:{context.source_mode}:{source_id}:g{context.generation}:"


def namespace_mb_cache_key(key: str, context: MbSourceContext | None = None) -> str:
    """Namespace source-dependent cache keys while retaining clear-prefix support."""
    from infrastructure.cache.cache_keys import musicbrainz_prefixes

    namespace = mb_cache_namespace(context)
    if not namespace:
        return key
    for prefix in musicbrainz_prefixes():
        if key.startswith(prefix):
            remainder = key[len(prefix) :]
            if remainder.startswith(namespace):
                return key
            return f"{prefix}{namespace}{remainder}"
    return key


def get_mb_response_context() -> MbSourceContext | None:
    return _mb_response_context.get()


def is_mb_source_current(context: MbSourceContext | None) -> bool:
    return bool(
        context is not None
        and context.generation == _mb_source_generation
        and context.source_url == _mb_api_base
        and context.source_mode == _mb_source_mode
        and context.source_id == _mb_source_id
        and (context.source_mode != "brainzmash" or _brainzmash_runtime_enabled)
    )


def normalize_mb_id(value: str | None) -> str:
    """Normalize a MusicBrainz entity ID for identity keys and lookups."""
    if not isinstance(value, str):
        return ""
    return value.strip().casefold()


def set_mb_api_base(
    url: str,
    *,
    source_mode: str = "official",
    source_id: str = "",
    generation: int | None = None,
    brainzmash_binding_valid: bool = False,
) -> None:
    global _brainzmash_runtime_enabled
    global _mb_api_base, _mb_source_generation, _mb_source_mode, _mb_source_id
    normalized = OFFICIAL_MB_API_BASE if source_mode == "official" else url.rstrip("/")
    changed = (
        normalized != _mb_api_base
        or source_mode != _mb_source_mode
        or source_id != _mb_source_id
    )
    if generation is not None:
        _mb_source_generation = generation
    elif changed:
        _mb_source_generation += 1
    _mb_api_base = normalized
    _mb_source_mode = source_mode
    _mb_source_id = source_id
    _brainzmash_runtime_enabled = (
        bool(brainzmash_binding_valid) if source_mode == "brainzmash" else False
    )


mb_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="musicbrainz",
    on_state_change=report_breaker_health(
        "musicbrainz",
        "metadata",
        message="MusicBrainz, our main source for music data, is having trouble - "
        "search and album or artist details may be incomplete for now.",
    ),
)
brainzmash_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="musicbrainz-brainzmash",
    on_state_change=report_breaker_health(
        "musicbrainz-brainzmash",
        "metadata",
        message="BrainzMash, our community source for music data, is having trouble - "
        "search and album or artist details may be incomplete for now.",
    ),
)


def get_mb_provider_circuit_breaker() -> CircuitBreaker:
    """Return the breaker governing the currently active MusicBrainz source."""
    return (
        brainzmash_circuit_breaker
        if _mb_source_mode == "brainzmash"
        else mb_circuit_breaker
    )


# MusicBrainz requires clients to make no more than one request per second:
# https://musicbrainz.org/doc/MusicBrainz_API/Rate_Limiting
mb_rate_limiter = TokenBucketRateLimiter(rate=1.0, capacity=1)

# BrainzMash has a shared sustained 10 requests/second policy with no burst
# capacity. The process-wide scheduler below serializes every BrainzMash wire
# attempt, including settings probes and retries.
brainzmash_rate_limiter = TokenBucketRateLimiter(rate=10.0, capacity=1)
brainzmash_probe_rate_limiter = brainzmash_rate_limiter

_BRAINZMASH_COOLDOWN_BASE_SECONDS = 1.0
_BRAINZMASH_MAX_COOLDOWN_SECONDS = 60.0
_BRAINZMASH_JITTER_FLOOR = 0.5


class _BrainzMashScheduler:
    """Serialize, pace, and cool down all BrainzMash attempts in this process."""

    def __init__(
        self,
        *,
        clock: Callable[[], float] = time.monotonic,
        random_fn: Callable[[], float] = random.random,
        sleep: Callable[[float], Awaitable[Any]] | None = None,
    ) -> None:
        self._lock = _ProcessWideAsyncLock()
        self._cooldown_lock = threading.Lock()
        self._clock = clock
        self._random = random_fn
        self._sleep = sleep or self._sleep_default
        self._cooldown_until = 0.0
        self._consecutive_no_retry_after = 0

    async def _sleep_default(self, seconds: float) -> None:
        await retry_module.asyncio.sleep(seconds)

    def note_cooldown(self, seconds: float | None) -> float:
        """Record one 429 and return the bounded delay selected for it."""
        try:
            parsed = float(seconds) if seconds is not None else None
        except (TypeError, ValueError):
            parsed = None
        valid_retry_after = parsed is not None and math.isfinite(parsed) and parsed >= 0
        with self._cooldown_lock:
            if valid_retry_after:
                self._consecutive_no_retry_after = 0
                delay = min(parsed, _BRAINZMASH_MAX_COOLDOWN_SECONDS)
            else:
                self._consecutive_no_retry_after += 1
                exponent = min(self._consecutive_no_retry_after - 1, 10)
                base_delay = min(
                    _BRAINZMASH_MAX_COOLDOWN_SECONDS,
                    _BRAINZMASH_COOLDOWN_BASE_SECONDS * (2**exponent),
                )
                try:
                    jitter = float(self._random())
                except (TypeError, ValueError):
                    jitter = 0.5
                if not math.isfinite(jitter):
                    jitter = 0.5
                jitter = min(1.0, max(0.0, jitter))
                delay = min(
                    _BRAINZMASH_MAX_COOLDOWN_SECONDS,
                    base_delay
                    * (
                        _BRAINZMASH_JITTER_FLOOR
                        + (1 - _BRAINZMASH_JITTER_FLOOR) * jitter
                    ),
                )
            self._cooldown_until = max(self._cooldown_until, self._clock() + delay)
            return delay

    def note_success(self) -> None:
        """A successful BrainzMash response clears the shared cooldown streak."""
        with self._cooldown_lock:
            self._consecutive_no_retry_after = 0
            self._cooldown_until = 0.0

    def cooldown_remaining(self) -> float:
        with self._cooldown_lock:
            return max(0.0, self._cooldown_until - self._clock())

    def reset(self) -> None:
        self.note_success()

    async def run(
        self,
        priority: RequestPriority,
        operation: Callable[[], Awaitable[T]],
        *,
        limiter: Any,
        on_result: Callable[[T], None] | None = None,
    ) -> T:
        async with self._lock:
            remaining = self.cooldown_remaining()
            if remaining > 0:
                await self._sleep(remaining)
            await limiter.acquire(priority=int(priority))
            result = await operation()
            if on_result is not None:
                on_result(result)
            return result


brainzmash_scheduler = _BrainzMashScheduler()

# P2 full-mirror tier (owner decision 2026-08-24): rate_limit=0 on a
# NON-official host means "Unlimited" - the client-side limiter is bypassed
# entirely for that host. Priority lanes, mb_deduplicator, and the circuit
# breaker below are NEVER relaxed; only this token bucket is skipped. The
# official-host defaults above stay pinned; appliers
# (musicbrainz_repository._apply_settings / settings_service.
# on_musicbrainz_settings_changed) flip this flag from saved settings.
_mb_limiter_bypassed = False


def set_mb_rate_limiter_bypass(bypass: bool) -> None:
    global _mb_limiter_bypassed
    _mb_limiter_bypassed = bypass


def mb_rate_limiter_bypassed() -> bool:
    return _mb_limiter_bypassed


class _SourceScopedRequestDeduplicator(RequestDeduplicator):
    async def dedupe(self, key: str, coro_factory: Callable[[], Awaitable[T]]) -> T:
        namespace = mb_cache_namespace()
        if namespace and not key.startswith(namespace):
            key = f"{namespace}{key}"
        return await super().dedupe(key, coro_factory)


mb_deduplicator = _SourceScopedRequestDeduplicator()

_http_client: httpx.AsyncClient | None = None
_brainzmash_http_client: httpx.AsyncClient | None = None


def set_mb_brainzmash_http_client(client: httpx.AsyncClient | None) -> None:
    global _brainzmash_http_client
    _brainzmash_http_client = client


def get_mb_brainzmash_http_client() -> httpx.AsyncClient:
    if _brainzmash_http_client is None:
        from infrastructure.http.client import get_brainzmash_http_client

        set_mb_brainzmash_http_client(get_brainzmash_http_client())
    assert _brainzmash_http_client is not None
    return _brainzmash_http_client


_MB_MAX_RETRY_AFTER_SECONDS = 60.0


def _parse_retry_after_seconds(value: str | None) -> float | None:
    if not value:
        return None

    try:
        seconds = float(value)
    except (TypeError, ValueError):
        try:
            parsed = parsedate_to_datetime(value)
            if parsed.tzinfo is None:
                parsed = parsed.replace(tzinfo=timezone.utc)
            seconds = (parsed - datetime.now(timezone.utc)).total_seconds()
        except (TypeError, ValueError, OverflowError):
            return None

    if not math.isfinite(seconds) or seconds < 0:
        return None
    return min(seconds, _MB_MAX_RETRY_AFTER_SECONDS)


def _note_brainzmash_response(response: httpx.Response) -> None:
    if response.status_code == 429:
        brainzmash_scheduler.note_cooldown(
            _parse_retry_after_seconds(response.headers.get("Retry-After"))
        )
    elif response.status_code == 200:
        brainzmash_scheduler.note_success()


_mb_probe_rate_limiter = TokenBucketRateLimiter(rate=1.0, capacity=1)


_BRAINZMASH_PROBE_ARTIST_ID = "5441c29d-3602-4898-b1a1-b77fa23b8e50"


async def mb_api_probe(
    api_url: str,
    *,
    params: dict[str, Any] | None = None,
    priority: RequestPriority = RequestPriority.USER_INITIATED,
    client: httpx.AsyncClient | None = None,
    brainzmash: bool = False,
    allow_unbound_brainzmash: bool = False,
    allow_quarantined_alternate: bool = False,
    source_context: MbSourceContext | None = None,
    admission_context: MbSourceContext | None = None,
    admission_check: Callable[[], bool] | None = None,
) -> httpx.Response:
    """Run the fixed probe route without changing the active runtime source."""
    endpoint = api_url.rstrip("/")
    admission_context = admission_context or capture_mb_source_context()
    if source_context is None:
        source_context = MbSourceContext(
            source_url=endpoint,
            generation=admission_context.generation,
            source_mode=admission_context.source_mode,
            source_id=f"probe-{uuid.uuid4().hex}",
        )
    if (
        not brainzmash
        and _mb_source_mode == "brainzmash"
        and not allow_quarantined_alternate
    ):
        raise ConfigurationError(
            "Alternative MusicBrainz probes are disabled while BrainzMash is active"
        )
    if brainzmash:
        if not _brainzmash_runtime_enabled and not allow_unbound_brainzmash:
            raise ConfigurationError("BrainzMash active binding is not valid")
        try:
            validate_brainzmash_url(api_url)
        except ValueError as exc:
            raise ExternalServiceError("BrainzMash endpoint is invalid") from exc
        endpoint = BRAINZMASH_ENDPOINT.rstrip("/")
    brainzmash_context = source_context

    clear_mb_response_context()
    priority_mgr = get_priority_queue()
    semaphore = await priority_mgr.acquire_slot(priority)
    async with semaphore:
        probe_client = (
            get_mb_brainzmash_http_client()
            if brainzmash
            else (client or get_mb_http_client())
        )
        request_params = {} if brainzmash else dict(params or {})
        request_params["fmt"] = "json"
        probe_path = (
            validate_brainzmash_path(f"/artist/{_BRAINZMASH_PROBE_ARTIST_ID}")
            if brainzmash
            else "/artist"
        )

        def admission_is_current() -> bool:
            try:
                source_current = is_mb_source_current(admission_context)
                if not source_current and allow_quarantined_alternate:
                    source_current = bool(
                        admission_check is not None
                        and not brainzmash
                        and not _brainzmash_runtime_enabled
                        and admission_context.source_mode == "brainzmash"
                        and admission_context.generation == _mb_source_generation
                        and admission_context.source_url == _mb_api_base
                        and admission_context.source_id == _mb_source_id
                    )
                if not source_current:
                    return False
                return bool(admission_check()) if admission_check is not None else True
            except Exception:  # noqa: BLE001 - stale admission fails closed
                return False

        async def request() -> httpx.Response:
            async with mb_source_commit_lock:
                if not admission_is_current():
                    raise _stale_source_error()
                response = await probe_client.get(
                    f"{endpoint}{probe_path}",
                    params=request_params,
                )
                if not admission_is_current():
                    raise _stale_source_error()
                return response

        try:
            if brainzmash:
                response = await brainzmash_scheduler.run(
                    priority,
                    request,
                    limiter=brainzmash_probe_rate_limiter,
                    on_result=_note_brainzmash_response,
                )
            else:
                await _mb_probe_rate_limiter.acquire(priority=int(priority))
                response = await request()
        except httpx.HTTPError:
            async with mb_source_commit_lock:
                if not admission_is_current():
                    raise _stale_source_error()
                record_provider_call(
                    "musicbrainz",
                    priority,
                    None,
                    brainzmash_context,
                )
            raise

    async with mb_source_commit_lock:
        if not admission_is_current():
            raise _stale_source_error()
        record_provider_call(
            "musicbrainz",
            priority,
            response.status_code,
            brainzmash_context,
        )
        record_rate_limit_headers("musicbrainz", response.headers)
        if response.status_code == 429:
            retry_after_seconds = _parse_retry_after_seconds(
                response.headers.get("Retry-After")
            )
            raise RateLimitedError(
                "BrainzMash rate limited (429)"
                if brainzmash
                else "MusicBrainz rate limited (429)",
                retry_after_seconds=retry_after_seconds,
            )
        if 300 <= response.status_code < 400:
            raise NonRetriableExternalServiceError(
                f"MusicBrainz probe redirect ({response.status_code})"
            )
        if brainzmash and response.status_code == 200:
            try:
                payload = _decode_json_response(response)
            except Exception as exc:  # noqa: BLE001 - strict probe contract
                raise InvalidExternalPayloadError(
                    "BrainzMash probe returned an unexpected payload"
                ) from exc
            if (
                not isinstance(payload, dict)
                or payload.get("id") != _BRAINZMASH_PROBE_ARTIST_ID
                or not isinstance(payload.get("name"), str)
                or not payload["name"].strip()
            ):
                raise InvalidExternalPayloadError(
                    "BrainzMash probe returned an unexpected payload"
                )
    return response


def _decode_json_response(response: httpx.Response) -> dict[str, Any]:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=dict[str, Any])
    return response.json()


def _decode_typed_response(response: httpx.Response, decode_type: type[T]) -> T:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=decode_type)
    return msgspec.convert(response.json(), type=decode_type)


def set_mb_http_client(client: httpx.AsyncClient) -> None:
    global _http_client
    _http_client = client


def get_mb_http_client() -> httpx.AsyncClient:
    if _mb_source_mode == "brainzmash":
        raise ConfigurationError(
            "The official MusicBrainz client is unavailable while BrainzMash is active"
        )
    if _http_client is None:
        raise RuntimeError("MusicBrainz HTTP client not initialized")
    return _http_client


async def _await_settled(publication: Awaitable[Any]) -> None:
    """Wait for a publication to finish even if the caller is cancelled."""
    task = (
        asyncio.Task(
            publication,
            loop=asyncio.get_running_loop(),
            eager_start=True,
        )
        if asyncio.iscoroutine(publication)
        else asyncio.ensure_future(publication)
    )
    try:
        await asyncio.shield(task)
    except asyncio.CancelledError:
        try:
            await task
        except asyncio.CancelledError:
            pass
        except Exception:  # noqa: BLE001 - inner publication must settle
            pass
        raise


async def mb_publish_if_current(
    context: MbSourceContext | None,
    publication: Callable[[], Awaitable[Any]],
) -> bool:
    """Publish under the process-wide source commit fence."""
    async with mb_source_commit_lock:
        if context is not None and not is_mb_source_current(context):
            return False
        token = _mb_operation_context.set(context)
        try:
            await _await_settled(publication())
        finally:
            _mb_operation_context.reset(token)
        return True


async def mb_cache_get_if_current(
    cache: Any,
    key: str,
    context: MbSourceContext | None,
) -> Any:
    """Read only the explicitly scoped cache entry for a current source."""
    if context is None or not is_mb_source_current(context):
        return None
    namespaced_key = namespace_mb_cache_key(key, context)
    if namespaced_key == key:
        return None
    token = _mb_operation_context.set(context)
    try:
        cached = await cache.get(namespaced_key)
    finally:
        _mb_operation_context.reset(token)
    if not is_mb_source_current(context):
        return None
    return cached


async def mb_cache_set_if_current(
    cache: Any,
    key: str,
    value: Any,
    *,
    ttl_seconds: int | float,
    context: MbSourceContext | None = None,
) -> bool:
    """Publish provider-derived cache data under the source commit fence."""
    return await mb_publish_if_current(
        context,
        lambda: cache.set(key, value, ttl_seconds=ttl_seconds),
    )


def _musicbrainz_breaker_for_request(*args: Any, **kwargs: Any) -> CircuitBreaker:
    context = kwargs.get("source_context")
    if context is None and len(args) >= 5:
        context = args[4]
    if isinstance(context, MbSourceContext) and context.source_mode == "brainzmash":
        return brainzmash_circuit_breaker
    return mb_circuit_breaker


@with_retry(
    max_attempts=3,
    circuit_breaker=_musicbrainz_breaker_for_request,
    retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
    non_breaking_exceptions=(InvalidExternalPayloadError,),
    non_retriable_exceptions=(
        InvalidExternalPayloadError,
        NonRetriableExternalServiceError,
        httpx.ConnectError,
        httpx.ProtocolError,
    ),
    retry_budget_seconds=2.5,
)
async def _mb_api_get_attempt(
    path: str,
    params: dict[str, Any] | None,
    priority: RequestPriority,
    decode_type: type[T] | None,
    source_context: MbSourceContext,
) -> dict[str, Any] | T:
    if source_context.source_mode == "brainzmash" and not _brainzmash_runtime_enabled:
        raise ConfigurationError("BrainzMash active binding is not valid")
    _mb_response_context.set(source_context)
    if not is_mb_source_current(source_context):
        raise _stale_source_error()

    brainzmash = source_context.source_mode == "brainzmash"
    if brainzmash:
        try:
            validate_brainzmash_url(source_context.source_url)
            safe_path = validate_brainzmash_path(path)
        except ValueError as exc:
            raise ConfigurationError(str(exc)) from exc
    else:
        safe_path = path

    priority_mgr = get_priority_queue()
    semaphore = await priority_mgr.acquire_slot(priority)
    async with semaphore:
        url = f"{source_context.source_url.rstrip('/')}{safe_path}"
        request_params = dict(params) if params else {}
        request_params["fmt"] = "json"

        async def request() -> httpx.Response:
            if not is_mb_source_current(source_context):
                raise _stale_source_error()
            client = (
                get_mb_brainzmash_http_client() if brainzmash else get_mb_http_client()
            )
            return await client.get(url, params=request_params)

        try:
            if brainzmash:
                response = await brainzmash_scheduler.run(
                    priority,
                    request,
                    limiter=brainzmash_rate_limiter,
                    on_result=_note_brainzmash_response,
                )
            else:
                if not _mb_limiter_bypassed:
                    await mb_rate_limiter.acquire(priority=int(priority))
                response = await request()
        except httpx.HTTPError:
            # Transport-level failure: record only the opaque source context.
            record_provider_call("musicbrainz", priority, None, source_context)
            raise
        record_provider_call(
            "musicbrainz", priority, response.status_code, source_context
        )
        # A source commit can land while the socket was in flight. Do not
        # decode or publish a response captured under the old generation.
        if not is_mb_source_current(source_context):
            raise _stale_source_error()
        # QW11 Part 2: free early-warning telemetry from the same response.
        # Separate gauge - this cannot perturb the call counters above.
        record_rate_limit_headers("musicbrainz", response.headers)
        if response.status_code == 404:
            empty_result: dict[str, Any] | T
            if decode_type is not None:
                empty_result = decode_type()
            else:
                empty_result = {}
            if not is_mb_source_current(source_context):
                raise _stale_source_error()
            return empty_result
        if response.status_code == 429:
            retry_after_seconds = _parse_retry_after_seconds(
                response.headers.get("Retry-After")
            )
            error = RateLimitedError(
                "BrainzMash rate limited (429)"
                if brainzmash
                else f"MusicBrainz rate limited (429): {safe_path}",
                retry_after_seconds=retry_after_seconds,
            )
            if brainzmash:
                setattr(error, "_retry_delay_managed", True)
                setattr(
                    error,
                    "_retry_delay_managed_seconds",
                    brainzmash_scheduler.cooldown_remaining(),
                )
            raise error
        if response.status_code == 503:
            raise ExternalServiceError(
                "BrainzMash temporarily unavailable (503)"
                if brainzmash
                else f"MusicBrainz rate limited (503): {safe_path}"
            )
        if 300 <= response.status_code < 400:
            raise NonRetriableExternalServiceError(
                "BrainzMash redirect rejected (3xx)"
                if brainzmash
                else f"MusicBrainz API redirect ({response.status_code}): {safe_path}"
            )
        if brainzmash and 400 <= response.status_code < 500:
            raise NonRetriableExternalServiceError(
                f"BrainzMash request rejected ({response.status_code})"
            )
        if response.status_code != 200:
            raise ExternalServiceError(
                "BrainzMash request failed"
                if brainzmash
                else f"MusicBrainz API error ({response.status_code}): {safe_path}"
            )
        try:
            if decode_type is not None:
                decoded: dict[str, Any] | T = _decode_typed_response(
                    response, decode_type
                )
            else:
                decoded = _decode_json_response(response)
            if not is_mb_source_current(source_context):
                raise _stale_source_error()
            return decoded
        except msgspec.ValidationError as exc:
            raise InvalidExternalPayloadError(
                "BrainzMash returned an unexpected payload shape"
                if brainzmash
                else f"MusicBrainz returned an unexpected payload shape for {safe_path}: {exc}"
            ) from exc
        except (msgspec.DecodeError, TypeError) as exc:
            raise InvalidExternalPayloadError(
                "BrainzMash returned an unparseable payload"
                if brainzmash
                else f"MusicBrainz returned an unparseable payload for {safe_path}: {exc}"
            ) from exc


async def mb_api_get(
    path: str,
    params: dict[str, Any] | None = None,
    priority: RequestPriority = RequestPriority.USER_INITIATED,
    decode_type: type[T] | None = None,
    *,
    source_context: MbSourceContext | None = None,
) -> dict[str, Any] | T:
    """Make one logical request against an explicitly captured source context."""
    source_context = source_context or capture_mb_source_context()
    if source_context.source_mode == "brainzmash" and not _brainzmash_runtime_enabled:
        raise ConfigurationError("BrainzMash active binding is not valid")
    clear_mb_response_context()
    return await _mb_api_get_attempt(
        path, params, priority, decode_type, source_context
    )


def should_include_release(
    release_group: dict[str, Any],
    included_secondary_types: set[str] | None = None,
    included_primary_types: set[str] | None = None,
) -> bool:
    if included_primary_types is not None:
        primary_type = (release_group.get("primary-type") or "").lower()
        if primary_type not in included_primary_types:
            return False

    secondary_types = set(
        map(str.lower, release_group.get("secondary-types", []) or [])
    )

    if included_secondary_types is None:
        exclude_types = {
            "compilation",
            "live",
            "remix",
            "soundtrack",
            "dj-mix",
            "mixtape/street",
            "demo",
        }
        return secondary_types.isdisjoint(exclude_types)

    if not secondary_types:
        return "studio" in included_secondary_types

    return bool(secondary_types.intersection(included_secondary_types))


def extract_artist_name(release_group: dict[str, Any]) -> str | None:
    artist_credit = release_group.get("artist-credit", [])
    if not isinstance(artist_credit, list) or not artist_credit:
        return None

    first_credit = artist_credit[0]
    if isinstance(first_credit, dict):
        return first_credit.get("name") or (first_credit.get("artist") or {}).get(
            "name"
        )
    return None


def parse_year(date_str: str | None) -> int | None:
    if not date_str:
        return None
    year = date_str.split("-", 1)[0]
    return int(year) if year.isdigit() else None


def get_score(item: dict[str, Any]) -> int:
    score = item.get("score") or item.get("ext:score")
    try:
        return int(score) if score else 0
    except (ValueError, TypeError):
        return 0


def select_edition(
    releases: list[dict[str, Any]], target_track_count: int
) -> str | None:
    """Single source of truth for best-edition selection inside one release
    group (F-062): every identification lane must resolve the SAME group to
    the SAME edition MBID.

    Ranking follows the approved NEW-DECISION-02 order
    (.dev-notes/LibraryAudit/DECISIONS-LIVE.md): evidence score ->
    Official status -> parsed date with explicit precision -> XW country
    preference -> release MBID. The evidence-score term is absent here BY
    CONSTRUCTION: this runs at recall time on release-group metadata,
    before any candidate release has been fetched and scored, so the
    shared key (repositories.edition_policy.recall_key) is the signed
    order minus that term.

    Editions with zero track-count are skipped CONSISTENTLY - they carry
    no medium data to match against and previously drifted the scanner/drop-import
    lane away from the native pipeline. Returns None only when no release carries
    a usable id or any track data at all.
    """
    scored: list[tuple] = []
    for release in releases:
        key = recall_key(release, target_track_count)
        if key is not None:
            scored.append(key)
    if not scored:
        return None
    return min(scored)[4]


def dedupe_by_id(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    seen = {}
    for item in items:
        item_id = item.get("id")
        normalized_id = normalize_mb_id(item_id)
        if normalized_id and normalized_id not in seen:
            seen[normalized_id] = item

    result = list(seen.values())
    result.sort(key=get_score, reverse=True)
    return result


def _normalize_tag_phrase(tag: str) -> str:
    return " ".join(tag.strip().lower().split())


_LUCENE_RESERVED = frozenset(r'+-&|!(){}[]^"~*?:\\/')


def escape_lucene_phrase(value: str) -> str:
    """Escape user text before placing it inside a Lucene field phrase."""

    return "".join(
        f"\\{character}" if character in _LUCENE_RESERVED else character
        for character in value
    )


def build_release_search_query(title: str, artist: str) -> str:
    """Build a release query live-verified against MusicBrainz WS/2 on 2026-08-13."""

    clauses = [f'release:"{escape_lucene_phrase(title)}"']
    if artist:
        clauses.append(f'artist:"{escape_lucene_phrase(artist)}"')
    return " AND ".join(clauses)


def build_release_group_search_query(title: str, artist: str) -> str:
    """Build a release-group query live-verified against MusicBrainz WS/2 on 2026-08-13."""

    escaped_title = escape_lucene_phrase(title)
    query = f'(releasegroup:"{escaped_title}" OR release:"{escaped_title}")'
    if artist:
        query += f' AND artist:"{escape_lucene_phrase(artist)}"'
    return query


def build_recording_search_query(title: str, artist: str) -> str:
    """Build a recording query using the same verified Lucene field escaping."""

    return (
        f'recording:"{escape_lucene_phrase(title)}" AND '
        f'artist:"{escape_lucene_phrase(artist)}"'
    )


def _escape_tag_phrase(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"')


def build_musicbrainz_tag_query(tag: str) -> str:
    base = _normalize_tag_phrase(tag)
    if not base:
        return 'tag:""^3'

    variants: list[str] = [base]
    seen = {base}

    def add_variant(value: str) -> None:
        normalized = _normalize_tag_phrase(value)
        if normalized and normalized not in seen:
            seen.add(normalized)
            variants.append(normalized)

    add_variant(base.replace("-", " "))
    add_variant(base.replace(" ", "-"))

    if "&" in base:
        add_variant(base.replace("&", " and "))
        add_variant(base.replace("&", " "))

    if " and " in base:
        add_variant(base.replace(" and ", " & "))
        add_variant(base.replace(" and ", " "))

    clauses = []
    for index, variant in enumerate(variants):
        escaped = _escape_tag_phrase(variant)
        boost = "^3" if index == 0 else "^2"
        clauses.append(f'tag:"{escaped}"{boost}')

    return " OR ".join(clauses)
