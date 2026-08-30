import asyncio
import math
import threading
from collections.abc import Awaitable, Callable
from contextvars import ContextVar
from dataclasses import dataclass
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from typing import Any, TypeVar
from urllib.parse import urlsplit

import httpx
import msgspec

from core.exceptions import (
    ExternalServiceError,
    InvalidExternalPayloadError,
    RateLimitedError,
)
from infrastructure.resilience.retry import with_retry, CircuitBreaker
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.queue.priority_queue import RequestPriority, get_priority_queue
from infrastructure.http.deduplication import RequestDeduplicator
from infrastructure.service_health import report_breaker_health
from infrastructure.observability.provider_counters import (
    record_provider_call,
    record_rate_limit_headers,
)
from repositories.edition_policy import recall_key

_mb_api_base: str = "https://musicbrainz.org/ws/2"
_mb_source_generation = 0


@dataclass(frozen=True)
class MbSourceContext:
    source_url: str
    generation: int


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


def capture_mb_source_context() -> MbSourceContext:
    """Capture the source generation before a provider service operation."""
    return MbSourceContext(
        source_url=_mb_api_base,
        generation=_mb_source_generation,
    )


def get_mb_response_context() -> MbSourceContext | None:
    return _mb_response_context.get()


def is_mb_source_current(context: MbSourceContext | None) -> bool:
    return bool(
        context is not None
        and context.generation == _mb_source_generation
        and context.source_url == _mb_api_base
    )


def normalize_mb_id(value: str | None) -> str:
    """Normalize a MusicBrainz entity ID for identity keys and lookups."""
    if not isinstance(value, str):
        return ""
    return value.strip().casefold()


def set_mb_api_base(url: str) -> None:
    global _mb_api_base, _mb_source_generation
    normalized = url.rstrip("/")
    if normalized != _mb_api_base:
        _mb_source_generation += 1
    _mb_api_base = normalized


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

# MusicBrainz requires clients to make no more than one request per second:
# https://musicbrainz.org/doc/MusicBrainz_API/Rate_Limiting
# A larger bucket preserves the average refill rate but still permits a cold-start burst.
mb_rate_limiter = TokenBucketRateLimiter(rate=1.0, capacity=1)

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


mb_deduplicator = RequestDeduplicator()

_http_client: httpx.AsyncClient | None = None
T = TypeVar("T")


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

    if not math.isfinite(seconds) or seconds <= 0:
        return None
    return min(seconds, _MB_MAX_RETRY_AFTER_SECONDS)


_mb_probe_rate_limiter = TokenBucketRateLimiter(rate=1.0, capacity=1)


async def mb_api_probe(
    api_url: str,
    *,
    params: dict[str, Any] | None = None,
    priority: RequestPriority = RequestPriority.USER_INITIATED,
    client: httpx.AsyncClient | None = None,
) -> httpx.Response:
    """Run one conservative settings probe without shared MB state.

    The probe shares the normal priority lane, HTTP client, and provider
    telemetry, but has its own capacity-one limiter and performs no retry,
    cache, source, or circuit-breaker mutation.
    """
    clear_mb_response_context()

    priority_mgr = get_priority_queue()
    semaphore = await priority_mgr.acquire_slot(priority)
    async with semaphore:
        await _mb_probe_rate_limiter.acquire(priority=int(priority))
        probe_client = client or get_mb_http_client()
        request_params = dict(params) if params else {}
        request_params["fmt"] = "json"
        try:
            response = await probe_client.get(
                f"{api_url.rstrip('/')}/artist", params=request_params
            )
        except httpx.HTTPError:
            record_provider_call("musicbrainz", priority, None)
            raise

    record_provider_call("musicbrainz", priority, response.status_code)
    record_rate_limit_headers("musicbrainz", response.headers)
    if response.status_code == 429:
        raise RateLimitedError(
            "MusicBrainz rate limited (429): /artist",
            retry_after_seconds=_parse_retry_after_seconds(
                response.headers.get("Retry-After")
            ),
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
        await _await_settled(publication())
        return True


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


@with_retry(
    max_attempts=3,
    circuit_breaker=mb_circuit_breaker,
    retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
    non_breaking_exceptions=(InvalidExternalPayloadError,),
    non_retriable_exceptions=(
        InvalidExternalPayloadError,
        httpx.ConnectError,
        httpx.ProtocolError,
    ),
    retry_budget_seconds=2.5,
)
async def mb_api_get(
    path: str,
    params: dict[str, Any] | None = None,
    priority: RequestPriority = RequestPriority.USER_INITIATED,
    decode_type: type[T] | None = None,
) -> dict[str, Any] | T:
    clear_mb_response_context()
    priority_mgr = get_priority_queue()
    semaphore = await priority_mgr.acquire_slot(priority)
    async with semaphore:
        if not _mb_limiter_bypassed:
            await mb_rate_limiter.acquire(priority=int(priority))
        client = get_mb_http_client()
        source_url = get_mb_api_base()
        _mb_response_context.set(
            MbSourceContext(source_url=source_url, generation=_mb_source_generation)
        )
        url = f"{source_url}{path}"
        request_params = dict(params) if params else {}
        request_params["fmt"] = "json"
        try:
            response = await client.get(url, params=request_params)
        except httpx.HTTPError:
            # transport-level failure (e.g. h2 stream reset): never produced a
            # response, so record http_error with no status and re-raise
            record_provider_call("musicbrainz", priority, None)
            raise
        record_provider_call("musicbrainz", priority, response.status_code)
        # QW11 Part 2: free early-warning telemetry from the same response.
        # Separate gauge - this cannot perturb the call counters above.
        record_rate_limit_headers("musicbrainz", response.headers)
        if response.status_code == 404:
            if decode_type is not None:
                return decode_type()
            return {}
        if response.status_code == 429:
            raise RateLimitedError(
                f"MusicBrainz rate limited (429): {path}",
                retry_after_seconds=_parse_retry_after_seconds(
                    response.headers.get("Retry-After")
                ),
            )
        if response.status_code == 503:
            # Keep 503 provider-directed delays out of the generic retry path:
            # its current budget cannot safely coordinate host changes.
            raise ExternalServiceError(f"MusicBrainz rate limited (503): {path}")
        if response.status_code != 200:
            raise ExternalServiceError(
                f"MusicBrainz API error ({response.status_code}): {path}"
            )
        try:
            if decode_type is not None:
                return _decode_typed_response(response, decode_type)
            return _decode_json_response(response)
        except msgspec.ValidationError as exc:
            # deterministic per payload (e.g. a field MusicBrainz sends as JSON
            # null), so it says nothing about service health and never counts
            # toward the circuit breaker
            raise InvalidExternalPayloadError(
                f"MusicBrainz returned an unexpected payload shape for {path}: {exc}"
            ) from exc
        except (msgspec.DecodeError, TypeError) as exc:
            # F-056: a malformed-but-deterministic payload says nothing about
            # service health - it must not count toward the breaker and must
            # not be retriable, or poison payloads churn forever as
            # PROVIDER_TEMPORARILY_UNAVAILABLE.
            raise InvalidExternalPayloadError(
                f"MusicBrainz returned an unparseable payload for {path}: {exc}"
            ) from exc


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
