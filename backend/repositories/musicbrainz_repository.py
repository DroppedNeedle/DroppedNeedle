import asyncio
import logging
from typing import Optional

import httpx

from models.search import SearchResult
from services.preferences_service import PreferencesService
from infrastructure.cache.memory_cache import CacheInterface
from core.exceptions import ConfigurationError
from repositories.musicbrainz_base import (
    brainzmash_rate_limiter,
    capture_mb_source_context,
    get_mb_source_mode,
    is_mb_source_current,
    mb_rate_limiter,
    set_mb_http_client,
    set_mb_brainzmash_http_client,
    set_mb_api_base,
    set_mb_rate_limiter_bypass,
)
from infrastructure.http.brainzmash_transport import validate_brainzmash_url
from repositories.musicbrainz_artist import MusicBrainzArtistMixin
from repositories.musicbrainz_album import MusicBrainzAlbumMixin

logger = logging.getLogger(__name__)


class MusicBrainzRepository(MusicBrainzArtistMixin, MusicBrainzAlbumMixin):
    def __init__(
        self,
        http_client: httpx.AsyncClient,
        cache: CacheInterface,
        preferences_service: PreferencesService,
        mb_canonical_store=None,
        brainzmash_http_client: httpx.AsyncClient | None = None,
    ):
        self._cache = cache
        self._preferences_service = preferences_service
        # ST2 P1: durable canonical maps (optional; None keeps every existing
        # test fixture working without SQLite). Production passes the shared
        # MbCanonicalStore singleton.
        self._mb_canonical_store = mb_canonical_store
        set_mb_http_client(http_client)
        if brainzmash_http_client is not None:
            set_mb_brainzmash_http_client(brainzmash_http_client)
        self._apply_settings()

    @property
    def mb_canonical_store(self):
        """Public read for collaborators (e.g. Spotify import ISRC banking)
        that ride the same durable tier without their own DI plumbing."""
        return self._mb_canonical_store

    def _apply_settings(self) -> None:
        from api.v1.schemas.settings import (
            _OFFICIAL_MB_CONCURRENT_SEARCHES,
            _OFFICIAL_MB_RATE_LIMIT,
            is_brainzmash_active_binding_valid,
            is_musicbrainz_rate_policy_public_host,
        )

        settings = self._preferences_service.get_musicbrainz_connection()
        brainzmash_binding_valid = True
        if settings.source_mode == "brainzmash":
            validate_brainzmash_url(settings.api_url)
            brainzmash_binding_valid = is_brainzmash_active_binding_valid(settings)
        official_host = is_musicbrainz_rate_policy_public_host(settings.api_url)
        rate_policy_public_host = settings.source_mode == "brainzmash" or official_host
        if settings.source_mode == "brainzmash":
            settings.rate_limit = 10.0
            settings.concurrent_searches = 1
            brainzmash_rate_limiter.update_rate(10.0)
            brainzmash_rate_limiter.update_capacity(1)
        elif official_host:
            settings.rate_limit = min(settings.rate_limit, _OFFICIAL_MB_RATE_LIMIT)
            settings.concurrent_searches = min(
                settings.concurrent_searches, _OFFICIAL_MB_CONCURRENT_SEARCHES
            )
            if settings.rate_limit <= 0:
                settings.rate_limit = _OFFICIAL_MB_RATE_LIMIT
        set_mb_api_base(
            settings.api_url,
            source_mode=settings.source_mode,
            source_id=settings.source_id,
            generation=settings.generation,
            brainzmash_binding_valid=brainzmash_binding_valid,
        )
        requested_bypass = settings.rate_limit == 0 and not rate_policy_public_host
        set_mb_rate_limiter_bypass(requested_bypass)
        if not requested_bypass:
            mb_rate_limiter.update_rate(
                _OFFICIAL_MB_RATE_LIMIT
                if settings.source_mode == "brainzmash"
                else settings.rate_limit
            )
        effective_capacity = (
            1 if rate_policy_public_host else settings.concurrent_searches
        )
        if mb_rate_limiter.capacity != effective_capacity:
            mb_rate_limiter.update_capacity(effective_capacity)

    async def search_grouped(
        self,
        query: str,
        limits: dict[str, int],
        buckets: Optional[list[str]] = None,
        included_secondary_types: Optional[set[str]] = None,
        included_primary_types: Optional[set[str]] = None,
    ) -> tuple[dict[str, list[SearchResult]], set[str]]:
        source_context = capture_mb_source_context()
        tasks = []
        task_keys = []

        if not buckets or "artists" in buckets:
            tasks.append(self.search_artists(query, limit=limits.get("artists", 10)))
            task_keys.append("artists")

        if not buckets or "albums" in buckets:
            tasks.append(
                self.search_albums(
                    query,
                    limit=limits.get("albums", 10),
                    included_secondary_types=included_secondary_types,
                    included_primary_types=included_primary_types,
                )
            )
            task_keys.append("albums")

        if not tasks:
            return {}, set()

        if get_mb_source_mode() == "brainzmash":
            results_list = []
            for task in tasks:
                try:
                    results_list.append(await task)
                except Exception as exc:  # noqa: BLE001 - preserve bucket isolation
                    results_list.append(exc)
        else:
            results_list = await asyncio.gather(*tasks, return_exceptions=True)

        results = {}
        failed_buckets = set()
        for key, result in zip(task_keys, results_list):
            if isinstance(result, Exception):
                logger.error("MusicBrainz grouped search failed")
                results[key] = []
                failed_buckets.add(key)
            else:
                results[key] = result

        if not is_mb_source_current(source_context):
            raise ConfigurationError("MusicBrainz source changed during grouped search")
        return results, failed_buckets
