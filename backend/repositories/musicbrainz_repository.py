import asyncio
import logging
from typing import Optional

import httpx

from models.search import SearchResult
from services.preferences_service import PreferencesService
from infrastructure.cache.memory_cache import CacheInterface
from repositories.musicbrainz_base import (
    mb_rate_limiter,
    set_mb_http_client,
    set_mb_api_base,
    set_mb_rate_limiter_bypass,
)
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
    ):
        self._cache = cache
        self._preferences_service = preferences_service
        # ST2 P1: durable canonical maps (optional; None keeps every existing
        # test fixture working without SQLite). Production passes the shared
        # MbCanonicalStore singleton.
        self._mb_canonical_store = mb_canonical_store
        set_mb_http_client(http_client)
        self._apply_settings()

    @property
    def mb_canonical_store(self):
        """Public read for collaborators (e.g. Spotify import ISRC banking)
        that ride the same durable tier without their own DI plumbing."""
        return self._mb_canonical_store

    def _apply_settings(self) -> None:
        from api.v1.schemas.settings import (
            is_musicbrainz_rate_policy_public_host,
            _OFFICIAL_MB_RATE_LIMIT,
            _OFFICIAL_MB_CONCURRENT_SEARCHES,
        )

        settings = self._preferences_service.get_musicbrainz_connection()
        rate_policy_public_host = is_musicbrainz_rate_policy_public_host(
            settings.api_url
        )
        if rate_policy_public_host:
            settings.rate_limit = min(settings.rate_limit, _OFFICIAL_MB_RATE_LIMIT)
            settings.concurrent_searches = min(
                settings.concurrent_searches, _OFFICIAL_MB_CONCURRENT_SEARCHES
            )
            # the Unlimited sentinel is off-official only
            if settings.rate_limit <= 0:
                settings.rate_limit = _OFFICIAL_MB_RATE_LIMIT
        set_mb_api_base(settings.api_url)
        if settings.rate_limit == 0:
            set_mb_rate_limiter_bypass(True)
        else:
            set_mb_rate_limiter_bypass(False)
            mb_rate_limiter.update_rate(settings.rate_limit)
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

        results_list = await asyncio.gather(*tasks, return_exceptions=True)

        results = {}
        failed_buckets = set()
        for key, result in zip(task_keys, results_list):
            if isinstance(result, Exception):
                logger.error(f"Search {key} failed: {result}")
                results[key] = []
                failed_buckets.add(key)
            else:
                results[key] = result

        return results, failed_buckets
