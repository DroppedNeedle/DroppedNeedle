"""``NewznabIndexer`` - the ``IndexerProtocol`` impl that fans one logical search
out across the N Newznab indexers the user configured (D6).

Per indexer: caps-gated query strategy (structured ``t=music`` only when caps
advertises audio-search with artist/album params, else free-text ``t=search`` -
DrunkenSlug takes the latter), a per-call timeout, rate-limit backoff, and a
short-TTL search cache so the fan-out + failover + auto-retry re-search don't each
re-hit the indexer's daily API budget. Results are pooled and **deduped by the
normalised (title, size) identity** (cross-indexer; ``guid`` is per-indexer), the
higher-priority indexer's copy winning. One indexer erroring never fails the
fan-out (``return_exceptions``). No ``from __future__ import annotations`` (the
conformance test compares real signatures).
"""

import asyncio
import logging
import time

from core.exceptions import NewznabApiError, NewznabAuthError, RateLimitedError
from models.common import ServiceStatus
from models.download_identity import usenet_identity
from repositories.protocols.indexer import IndexerResult, UsenetRelease

from .newznab_client import NewznabClient
from .newznab_models import NewznabCaps

logger = logging.getLogger(__name__)

_UNKNOWN_FUNCTION = 202  # Newznab "No such function" - fall back t=music -> t=search

# Characters a scene release name never carries. A canonical MusicBrainz title
# like "Honestly, Nevermind" or "Man's Best Friend" must also be searchable
# without them: live-verified NZBGeek behavior returns 0 releases for the comma
# query and 3 lossless for the stripped one (#259). Typographic quotes fold to
# ASCII first so the strip covers them; the survivors are REMOVED (not spaced),
# so "Man's" becomes "Mans" exactly the way scene names drop the apostrophe.
_TYPOGRAPHIC_QUOTES = {
    "\u2018": "'",
    "\u2019": "'",
    "\u201a": "'",
    "\u201b": "'",
    "\u2032": "'",
    "\u2035": "'",
    "`": "'",
    "\u00b4": "'",
    "\u201c": '"',
    "\u201d": '"',
    "\u00ab": '"',
    "\u00bb": '"',
    "\u201e": '"',
    "\u201f": '"',
}
_QUERY_STRIP_CHARS = frozenset({",", "'", '"', "?", "!", ";", ":", "&"})


def normalize_newznab_query(query: str) -> str:
    """Punctuation-normalized free-text query rung (#259, decision LADDER).

    Outbound-query-only: the canonical MusicBrainz title still flows untouched
    to the scorer/import and to the structured ``t=music`` params. Idempotent
    and a no-op on punctuation-free input.
    """
    folded = "".join(_TYPOGRAPHIC_QUOTES.get(ch, ch) for ch in query)
    stripped = "".join("" if ch in _QUERY_STRIP_CHARS else ch for ch in folded)
    return " ".join(stripped.split())


class NewznabIndexerEntry:
    """One configured indexer paired with its HTTP client (D6). Not serialised -
    a runtime holder the DI provider builds from ``NewznabIndexerSettings``."""

    def __init__(
        self,
        client: NewznabClient,
        *,
        indexer_id: str,
        name: str,
        categories: list[int],
        enabled: bool = True,
        priority: int = 1,
        limit: int = 100,
    ) -> None:
        self.client = client
        self.id = indexer_id
        self.name = name
        self.categories = categories
        self.enabled = enabled
        self.priority = priority
        self.limit = limit


class NewznabIndexer:
    def __init__(
        self,
        entries: list[NewznabIndexerEntry],
        *,
        search_cache_ttl: float = 300.0,
        caps_cache_ttl: float = 7 * 24 * 3600.0,
        rate_limit_backoff: float = 300.0,
        per_indexer_timeout: float = 30.0,
    ) -> None:
        # Higher priority (lower number) first, so dedup keeps the preferred copy.
        self._entries = sorted(entries, key=lambda e: e.priority)
        self._search_cache_ttl = search_cache_ttl
        self._caps_cache_ttl = caps_cache_ttl
        self._rate_limit_backoff = rate_limit_backoff
        self._timeout = per_indexer_timeout
        self._caps_cache: dict[str, tuple[float, NewznabCaps]] = {}
        self._search_cache: dict[tuple[str, str], tuple[float, list[UsenetRelease]]] = {}
        self._backoff_until: dict[str, float] = {}

    @property
    def indexer_name(self) -> str:
        return "usenet"

    def is_configured(self) -> bool:
        return any(e.enabled for e in self._entries)

    async def health_check(self) -> ServiceStatus:
        enabled = [e for e in self._entries if e.enabled]
        if not enabled:
            return ServiceStatus(status="error", message="No indexers configured")
        reachable = 0
        version: str | None = None
        for entry in enabled:
            try:
                caps = await entry.client.caps(timeout=self._timeout)
            except Exception as exc:  # noqa: BLE001 - health check never raises
                logger.warning("newznab health: indexer %s unreachable: %s", entry.name, exc)
                continue
            reachable += 1
            version = version or caps.server_version
        if reachable:
            return ServiceStatus(
                status="ok",
                version=version,
                message=f"{reachable}/{len(enabled)} indexer(s) reachable",
            )
        return ServiceStatus(status="error", message="No indexer reachable")

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
        releases = await self._search_with_ladder(
            query, artist=artist_name, album=album_title, year=year, timeout=timeout
        )
        return [IndexerResult(source="usenet", usenet=r) for r in releases]

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]:
        # There's no reliable single-track Usenet search; the orchestrator's D4 path
        # resolves to the album. Here we just free-text search artist+track (album is
        # None so the structured music path is never taken for a track).
        query = f"{artist_name} {track_title}".strip()
        releases = await self._search_with_ladder(
            query, artist=artist_name, album=None, year=None, timeout=timeout
        )
        return [IndexerResult(source="usenet", usenet=r) for r in releases]

    async def _search_with_ladder(
        self,
        query: str,
        *,
        artist: str,
        album: str | None,
        year: int | None,
        timeout: float,
    ) -> list[UsenetRelease]:
        """Canonical query first; on a genuine empty, one normalized retry (#259).

        The retry fires only when every enabled indexer actually ANSWERED (at
        least one successful empty response and none rate-limited, auth-failed,
        or errored): an indexer that sat out in backoff may hold the release,
        so an empty pool with a silent indexer is not proof the query is dead
        and a second query would only spend API budget. The retry rung forces
        free-text ``t=search`` even when caps advertises ``t=music`` - the
        structured params stay canonical by decision, so re-sending them would
        be a byte-identical repeat; the normalized free text is the new signal.
        A fresh cache key (``(entry.id, query)``) keeps the rungs distinct.
        """
        releases, clean = await self._fan_out(
            query, artist=artist, album=album, year=year, timeout=timeout
        )
        if releases:
            return releases
        normalized = normalize_newznab_query(query)
        if normalized == query or not clean:
            return releases
        logger.info(
            "newznab.query_normalized_retry",
            extra={"query": query, "normalized_query": normalized},
        )
        releases, _ = await self._fan_out(
            normalized,
            artist=artist,
            album=album,
            year=year,
            timeout=timeout,
            text_only=True,
        )
        return releases

    async def _fan_out(
        self,
        query: str,
        *,
        artist: str,
        album: str | None,
        year: int | None,
        timeout: float,
        text_only: bool = False,
    ) -> tuple[list[UsenetRelease], bool]:
        """Fan one logical search out across the enabled indexers.

        Returns ``(pooled_releases, clean)`` where ``clean`` is True only when
        every enabled indexer returned a successful response (even an empty
        one). A backoff skip, auth failure, rate limit, or per-indexer
        exception flips it False - the caller treats that pool as untrustworthy
        for ladder purposes. Internal-only; the public search signatures stay
        protocol-conformant.
        """
        enabled = [e for e in self._entries if e.enabled]
        if not enabled:
            return [], False
        per_call = min(timeout, self._timeout)
        results = await asyncio.gather(
            *(
                self._search_one(
                    e,
                    query,
                    artist=artist,
                    album=album,
                    year=year,
                    timeout=per_call,
                    text_only=text_only,
                )
                for e in enabled
            ),
            return_exceptions=True,
        )
        pooled: list[UsenetRelease] = []
        clean = True
        for entry, res in zip(enabled, results):
            if isinstance(res, Exception):
                logger.warning("newznab indexer %s search failed: %s", entry.name, res)
                clean = False
                continue
            releases, ok = res
            if not ok:
                clean = False
            pooled.extend(releases)
        return self._dedup(pooled), clean

    async def _search_one(
        self,
        entry: NewznabIndexerEntry,
        query: str,
        *,
        artist: str,
        album: str | None,
        year: int | None,
        timeout: float,
        text_only: bool = False,
    ) -> tuple[list[UsenetRelease], bool]:
        """One indexer's search. The bool reports a successful indexer ANSWER
        (results or a genuine empty); backoff skips, rate limits, and auth
        failures report False so the ladder never retries on their silence."""
        now = time.monotonic()
        if self._backoff_until.get(entry.id, 0.0) > now:
            logger.info("newznab indexer %s in rate-limit backoff; skipping", entry.name)
            return [], False
        cache_key = (entry.id, query)
        cached = self._search_cache.get(cache_key)
        if cached is not None and cached[0] > now:
            return cached[1], True
        # Opportunistically evict expired entries so the cache can't grow unbounded over
        # a long-running process doing many distinct album searches.
        if len(self._search_cache) > 256:
            self._search_cache = {k: v for k, v in self._search_cache.items() if v[0] > now}

        caps = await self._caps(entry, timeout)
        limit = min(entry.limit or caps.limit_max, caps.limit_max)
        audio_params = set(caps.audio_search_params)
        use_music = (
            not text_only
            and bool(album)
            and caps.supports_audio_search
            and {"artist", "album"} <= audio_params
        )
        # Only send year when the indexer advertises it (Lidarr/Prowlarr send only
        # advertised params); an unadvertised param can be rejected or silently widen.
        year_param = year if "year" in audio_params else None
        try:
            if use_music:
                releases, _limits = await self._music_or_fallback(
                    entry, artist, album or "", query, year_param, limit, timeout
                )
            else:
                releases, _limits = await entry.client.search(
                    query, entry.categories, limit=limit, timeout=timeout
                )
        except RateLimitedError as exc:
            backoff = exc.retry_after_seconds or self._rate_limit_backoff
            self._backoff_until[entry.id] = now + backoff
            logger.warning(
                "newznab indexer %s rate-limited; backing off %.0fs", entry.name, backoff
            )
            return [], False
        except NewznabAuthError as exc:
            logger.warning("newznab indexer %s auth failed: %s", entry.name, exc)
            return [], False

        self._search_cache[cache_key] = (now + self._search_cache_ttl, releases)
        return releases, True

    async def _music_or_fallback(
        self,
        entry: NewznabIndexerEntry,
        artist: str,
        album: str,
        query: str,
        year: int | None,
        limit: int,
        timeout: float,
    ):
        # Decision A4: the structured artist/album params stay canonical - only
        # the free-text ``query`` normalizes, and the 202 fallback below
        # inherits whichever rung query the caller passed in.
        try:
            return await entry.client.music_search(
                artist, album, entry.categories, year=year, limit=limit, timeout=timeout
            )
        except NewznabApiError as exc:
            if exc.code == _UNKNOWN_FUNCTION:
                logger.info(
                    "newznab indexer %s lacks t=music; falling back to t=search", entry.name
                )
                return await entry.client.search(
                    query, entry.categories, limit=limit, timeout=timeout
                )
            raise

    async def _caps(self, entry: NewznabIndexerEntry, timeout: float) -> NewznabCaps:
        now = time.monotonic()
        cached = self._caps_cache.get(entry.id)
        if cached is not None and cached[0] > now:
            return cached[1]
        try:
            caps = await entry.client.caps(timeout=timeout)
        except Exception as exc:  # noqa: BLE001 - permissive default (Lidarr/Prowlarr)
            logger.warning("newznab caps fetch failed for %s: %s", entry.name, exc)
            return NewznabCaps()  # working-indexer defaults: text q, limit 100
        self._caps_cache[entry.id] = (now + self._caps_cache_ttl, caps)
        return caps

    def _dedup(self, releases: list[UsenetRelease]) -> list[UsenetRelease]:
        """Pool to one list, deduped by the cross-indexer (title, size) identity -
        the same key the Usenet quarantine uses. Higher-priority indexers come first
        in ``releases`` (caller order), so first-seen-wins keeps the preferred copy."""
        seen: set[str] = set()
        out: list[UsenetRelease] = []
        dropped = 0
        for release in releases:
            key = usenet_identity(release.title, release.size_bytes)
            if key in seen:
                dropped += 1
                continue
            seen.add(key)
            out.append(release)
        if dropped:
            logger.info("newznab fan-out deduped %d cross-indexer duplicate(s)", dropped)
        return out
