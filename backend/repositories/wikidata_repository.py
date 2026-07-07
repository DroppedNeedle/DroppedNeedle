import httpx
import logging
import msgspec
import re
from typing import TypeVar
from urllib.parse import quote
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.cache.cache_keys import (
    wikipedia_extract_key,
    wikidata_artist_image_key,
)
from infrastructure.resilience.retry import with_retry, CircuitBreaker
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.service_health import report_breaker_health

logger = logging.getLogger(__name__)

_SOURCE = "wikidata"


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))

T = TypeVar("T")


class _WikidataSiteLink(msgspec.Struct):
    title: str | None = None


class _WikidataValue(msgspec.Struct):
    value: str | None = None


class _WikidataSnak(msgspec.Struct):
    datavalue: _WikidataValue | None = None


class _WikidataClaim(msgspec.Struct):
    mainsnak: _WikidataSnak | None = None


class _WikidataEntity(msgspec.Struct):
    sitelinks: dict[str, _WikidataSiteLink] = {}


class _WikidataEntityResponse(msgspec.Struct):
    entities: dict[str, _WikidataEntity] = {}


class _WikidataClaimsResponse(msgspec.Struct):
    claims: dict[str, list[_WikidataClaim]] = {}


class _WikipediaPage(msgspec.Struct):
    pageid: int | None = None
    extract: str | None = None


class _WikipediaQuery(msgspec.Struct):
    pages: dict[str, _WikipediaPage] = {}


class _WikipediaQueryResponse(msgspec.Struct):
    query: _WikipediaQuery | None = None


class _CommonsImageInfo(msgspec.Struct):
    url: str | None = None


class _CommonsPage(msgspec.Struct):
    imageinfo: list[_CommonsImageInfo] = []


class _CommonsQuery(msgspec.Struct):
    pages: dict[str, _CommonsPage] = {}


class _CommonsQueryResponse(msgspec.Struct):
    query: _CommonsQuery | None = None


def _decode_json_response(response: httpx.Response, decode_type: type[T]) -> T:
    content = getattr(response, "content", None)
    if isinstance(content, (bytes, bytearray, memoryview)):
        return msgspec.json.decode(content, type=decode_type)
    return msgspec.convert(response.json(), type=decode_type)

_wikidata_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="wikidata",
    on_state_change=report_breaker_health(
        "wikidata",
        "artist info",
        message="Artist bios and images (Wikipedia) are temporarily unavailable.",
    ),
)


class WikidataRepository:
    def __init__(self, http_client: httpx.AsyncClient, cache: CacheInterface):
        self._client = http_client
        self._cache = cache
    
    @staticmethod
    def _extract_wikidata_id(url: str) -> str | None:
        match = re.search(r'/wiki/(Q\d+)', url)
        return match.group(1) if match else None
    
    @staticmethod
    def _extract_wikipedia_title(url: str) -> str | None:
        match = re.search(r'/wiki/(.+)$', url)
        return match.group(1) if match else None
    
    @with_retry(
        max_attempts=3,
        base_delay=0.5,
        max_delay=3.0,
        circuit_breaker=_wikidata_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError,)
    )
    async def _get_wikipedia_title_from_wikidata(
        self,
        wikidata_id: str,
        lang: str = "en"
    ) -> str | None:
        try:
            api_url = f"https://www.wikidata.org/wiki/Special:EntityData/{wikidata_id}.json"
            response = await self._client.get(api_url)
            
            if response.status_code != 200:
                return None
            
            data = _decode_json_response(response, _WikidataEntityResponse)
            entity = data.entities.get(wikidata_id)
            if entity is None:
                return None
            wiki_data = entity.sitelinks.get(f"{lang}wiki")
            return wiki_data.title if wiki_data else None
        
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get Wikipedia title for {wikidata_id}: {e}")
            _record_degradation(f"Failed to get Wikipedia title for {wikidata_id}: {e}")
            return None
    
    @with_retry(
        max_attempts=3,
        base_delay=0.5,
        max_delay=3.0,
        circuit_breaker=_wikidata_circuit_breaker,
        retriable_exceptions=(httpx.HTTPError,)
    )
    async def _fetch_wikipedia_extract(self, page_title: str, lang: str = "en") -> str | None:
        try:
            api_url = (
                f"https://{lang}.wikipedia.org/w/api.php"
                f"?action=query&titles={quote(page_title)}"
                f"&prop=extracts&exintro=1&explaintext=1&format=json"
            )
            
            response = await self._client.get(api_url)
            if response.status_code != 200:
                return None
            
            data = _decode_json_response(response, _WikipediaQueryResponse)
            pages = data.query.pages if data.query else {}

            for page_data in pages.values():
                if (page_data.pageid or -1) < 0:
                    return None

                if extract := page_data.extract:
                    return extract
            
            return None
        
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to fetch Wikipedia extract: {e}")
            _record_degradation(f"Failed to fetch Wikipedia extract: {e}")
            return None
    
    async def get_wikipedia_extract(self, wikipedia_url: str, lang: str = "en") -> str | None:
        cache_key = wikipedia_extract_key(wikipedia_url)
        
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        
        try:
            if wikidata_id := self._extract_wikidata_id(wikipedia_url):
                page_title = await self._get_wikipedia_title_from_wikidata(wikidata_id, lang)
                if not page_title:
                    return None
            
            elif page_title := self._extract_wikipedia_title(wikipedia_url):
                pass
            
            else:
                return None
            
            extract = await self._fetch_wikipedia_extract(page_title, lang)
            
            if extract:
                await self._cache.set(cache_key, extract, ttl_seconds=604800)
            
            return extract
        
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get Wikipedia extract from {wikipedia_url}: {e}")
            _record_degradation(f"Failed to get Wikipedia extract: {e}")
            return None
    
    def get_wikidata_id_from_url(self, wikidata_url: str) -> str | None:
        return self._extract_wikidata_id(wikidata_url)
    
    async def get_artist_image_from_wikidata(self, wikidata_id: str) -> str | None:
        cache_key = wikidata_artist_image_key(wikidata_id)
        
        cached = await self._cache.get(cache_key)
        if cached is not None:
            return cached
        
        try:
            api_url = (
                f"https://www.wikidata.org/w/api.php"
                f"?action=wbgetclaims&entity={wikidata_id}&property=P18&format=json"
            )
            response = await self._client.get(api_url)
            
            if response.status_code != 200:
                return None
            
            data = _decode_json_response(response, _WikidataClaimsResponse)
            image_claims = data.claims.get("P18", [])
            if not image_claims:
                return None

            first_claim = image_claims[0]
            image_filename = (
                first_claim.mainsnak.datavalue.value
                if first_claim.mainsnak and first_claim.mainsnak.datavalue
                else None
            )
            if not image_filename:
                return None
            
            commons_url = (
                f"https://commons.wikimedia.org/w/api.php"
                f"?action=query&titles=File:{quote(image_filename)}"
                f"&prop=imageinfo&iiprop=url&format=json"
            )
            
            response = await self._client.get(commons_url)
            if response.status_code != 200:
                return None
            
            commons_data = _decode_json_response(response, _CommonsQueryResponse)
            pages = commons_data.query.pages if commons_data.query else {}

            for page_data in pages.values():
                if page_data.imageinfo:
                    image_url = page_data.imageinfo[0].url
                    if image_url:
                        await self._cache.set(cache_key, image_url, ttl_seconds=86400)
                    return image_url
            
            return None
        
        except Exception as e:  # noqa: BLE001
            logger.error(f"Failed to get image for Wikidata {wikidata_id}: {e}")
            _record_degradation(f"Failed to get Wikidata artist image: {e}")
            return None
