from typing import Any, TypeVar

import httpx
import msgspec

from core.exceptions import ExternalServiceError
from infrastructure.resilience.retry import with_retry, CircuitBreaker
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.queue.priority_queue import RequestPriority, get_priority_queue
from infrastructure.http.deduplication import RequestDeduplicator
from infrastructure.service_health import report_breaker_health

_mb_api_base: str = "https://musicbrainz.org/ws/2"


def get_mb_api_base() -> str:
    return _mb_api_base


def set_mb_api_base(url: str) -> None:
    global _mb_api_base
    _mb_api_base = url.rstrip("/")

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

mb_deduplicator = RequestDeduplicator()

_http_client: httpx.AsyncClient | None = None
T = TypeVar("T")


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


@with_retry(
    max_attempts=3,
    circuit_breaker=mb_circuit_breaker,
    retriable_exceptions=(httpx.HTTPError, ExternalServiceError),
)
async def mb_api_get(
    path: str,
    params: dict[str, Any] | None = None,
    priority: RequestPriority = RequestPriority.USER_INITIATED,
    decode_type: type[T] | None = None,
) -> dict[str, Any] | T:
    priority_mgr = get_priority_queue()
    semaphore = await priority_mgr.acquire_slot(priority)
    async with semaphore:
        await mb_rate_limiter.acquire()
        client = get_mb_http_client()
        url = f"{get_mb_api_base()}{path}"
        request_params = dict(params) if params else {}
        request_params["fmt"] = "json"
        response = await client.get(url, params=request_params)
        if response.status_code == 404:
            if decode_type is not None:
                return decode_type()
            return {}
        if response.status_code == 503:
            raise ExternalServiceError(f"MusicBrainz rate limited (503): {path}")
        if response.status_code != 200:
            raise ExternalServiceError(
                f"MusicBrainz API error ({response.status_code}): {path}"
            )
        try:
            if decode_type is not None:
                return _decode_typed_response(response, decode_type)
            return _decode_json_response(response)
        except (msgspec.DecodeError, msgspec.ValidationError, TypeError) as exc:
            raise ExternalServiceError(f"MusicBrainz returned invalid JSON payload for {path}: {exc}") from exc


def should_include_release(
    release_group: dict[str, Any],
    included_secondary_types: set[str] | None = None,
    included_primary_types: set[str] | None = None,
) -> bool:
    if included_primary_types is not None:
        primary_type = (release_group.get("primary-type") or "").lower()
        if primary_type not in included_primary_types:
            return False

    secondary_types = set(map(str.lower, release_group.get("secondary-types", []) or []))

    if included_secondary_types is None:
        exclude_types = {"compilation", "live", "remix", "soundtrack", "dj-mix", "mixtape/street", "demo"}
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
        return first_credit.get("name") or (first_credit.get("artist") or {}).get("name")
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


def dedupe_by_id(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    seen = {}
    for item in items:
        item_id = item.get("id")
        if item_id and item_id not in seen:
            seen[item_id] = item

    result = list(seen.values())
    result.sort(key=get_score, reverse=True)
    return result


def _normalize_tag_phrase(tag: str) -> str:
    return " ".join(tag.strip().lower().split())


def _escape_lucene_phrase(value: str) -> str:
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
        escaped = _escape_lucene_phrase(variant)
        boost = "^3" if index == 0 else "^2"
        clauses.append(f'tag:"{escaped}"{boost}')

    return " OR ".join(clauses)
