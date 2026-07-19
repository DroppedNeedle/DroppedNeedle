"""``ProwlarrIndexer`` - one ``IndexerProtocol`` impl over a single Prowlarr
connection.

Prowlarr aggregates every indexer the user manages there (usenet AND torrent,
public and private trackers) behind one URL + API key, so this replaces the
per-indexer Newznab fan-out with a single call and additionally feeds the
torrent source: one ``search_album`` returns BOTH ``usenet`` and ``torrent``
arms of ``IndexerResult``, and each strategy filters the arm it owns.

Results are deduped per-protocol by the normalised (title, size) identity (the
same key quarantine uses). A short-TTL search cache keeps failover + auto-retry
re-searches from re-hitting indexer API budgets (mirrors ``NewznabIndexer``).
No ``from __future__ import annotations`` (the conformance test compares real
signatures).
"""

import logging
import time
from datetime import datetime

from models.common import ServiceStatus
from models.download_identity import usenet_identity
from repositories.protocols.indexer import IndexerResult, TorrentRelease, UsenetRelease

from .prowlarr_client import ProwlarrApiError, ProwlarrClient
from .prowlarr_models import ProwlarrSearchResult

logger = logging.getLogger(__name__)


def _parse_publish_date(value: str) -> float | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


class ProwlarrIndexer:
    def __init__(
        self,
        client: ProwlarrClient,
        *,
        categories: list[int],
        enabled: bool = True,
        search_cache_ttl: float = 300.0,
        limit: int = 100,
    ) -> None:
        self._client = client
        self._categories = categories
        self._enabled = enabled
        self._search_cache_ttl = search_cache_ttl
        self._limit = limit
        self._search_cache: dict[str, tuple[float, list[IndexerResult]]] = {}

    @property
    def indexer_name(self) -> str:
        return "prowlarr"

    def is_configured(self) -> bool:
        return self._enabled

    async def health_check(self) -> ServiceStatus:
        try:
            status = await self._client.system_status()
        except Exception as exc:  # noqa: BLE001 - health check never raises
            return ServiceStatus(status="error", message=str(exc))
        return ServiceStatus(
            status="ok",
            version=status.version or None,
            message=f"Prowlarr {status.version}" if status.version else "Prowlarr",
        )

    async def search_album(
        self,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        query = f"{artist_name} {album_title}".strip()
        return await self._search(query, timeout=timeout)

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        query = f"{artist_name} {track_title}".strip()
        return await self._search(query, timeout=timeout)

    async def _search(self, query: str, *, timeout: float) -> list[IndexerResult]:
        now = time.monotonic()
        cached = self._search_cache.get(query)
        if cached is not None and cached[0] > now:
            return cached[1]
        if len(self._search_cache) > 256:
            self._search_cache = {
                k: v for k, v in self._search_cache.items() if v[0] > now
            }
        try:
            rows = await self._client.search(
                query, self._categories, limit=self._limit, timeout=timeout
            )
        except ProwlarrApiError as exc:
            logger.warning("prowlarr search failed: %s", exc)
            return []
        results = self._convert(rows)
        self._search_cache[query] = (now + self._search_cache_ttl, results)
        return results

    def _convert(self, rows: list[ProwlarrSearchResult]) -> list[IndexerResult]:
        """Map Prowlarr rows to tagged ``IndexerResult``s, deduped per-protocol by
        the (title, size) identity - first-seen-wins (Prowlarr's own ordering)."""
        out: list[IndexerResult] = []
        seen: set[tuple[str, str]] = set()
        dropped = 0
        for row in rows:
            protocol = row.protocol.lower()
            key = (protocol, usenet_identity(row.title, row.size))
            if key in seen:
                dropped += 1
                continue
            seen.add(key)
            categories = [c.id for c in row.categories if c.id]
            if protocol == "usenet":
                if not row.download_url:
                    continue
                out.append(
                    IndexerResult(
                        source="usenet",
                        usenet=UsenetRelease(
                            indexer_id=str(row.indexer_id),
                            indexer_name=row.indexer or "prowlarr",
                            guid=row.guid,
                            title=row.title,
                            nzb_url=self._client.absolute_url(row.download_url),
                            size_bytes=row.size,
                            category_ids=categories,
                            grabs=row.grabs,
                            files=row.files,
                            usenet_date=_parse_publish_date(row.publish_date),
                        ),
                    )
                )
            elif protocol == "torrent":
                if not row.download_url and not row.magnet_url:
                    continue
                out.append(
                    IndexerResult(
                        source="torrent",
                        torrent=TorrentRelease(
                            indexer_id=str(row.indexer_id),
                            indexer_name=row.indexer or "prowlarr",
                            guid=row.guid,
                            title=row.title,
                            download_url=self._client.absolute_url(row.download_url),
                            magnet_url=row.magnet_url,
                            info_hash=row.info_hash,
                            size_bytes=row.size,
                            category_ids=categories,
                            seeders=row.seeders,
                            leechers=row.leechers,
                            grabs=row.grabs,
                            publish_date=_parse_publish_date(row.publish_date),
                        ),
                    )
                )
        if dropped:
            logger.info("prowlarr search deduped %d duplicate release(s)", dropped)
        return out
