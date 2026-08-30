import asyncio
import logging
from typing import Any
from urllib.parse import quote_plus

from api.v1.schemas.discover import DiscoverQueueEnrichment
from infrastructure.cache.cache_keys import DISCOVER_QUEUE_ENRICH_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.validators import clean_lastfm_bio
from repositories.protocols import (
    ListenBrainzRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
    LastFmRepositoryProtocol,
)
from repositories.musicbrainz_base import (
    MbSourceContext,
    capture_mb_source_context,
    is_mb_source_current,
    mb_publish_if_current,
    normalize_mb_id,
)
from services.discover.integration_helpers import IntegrationHelpers

logger = logging.getLogger(__name__)


class QueueEnrichmentService:
    def __init__(
        self,
        musicbrainz_repo: MusicBrainzRepositoryProtocol,
        listenbrainz_repo: ListenBrainzRepositoryProtocol,
        preferences_service: Any,
        integration: IntegrationHelpers,
        memory_cache: CacheInterface | None = None,
        wikidata_repo: Any = None,
        lastfm_repo: LastFmRepositoryProtocol | None = None,
    ) -> None:
        self._mb_repo = musicbrainz_repo
        self._lb_repo = listenbrainz_repo
        self._preferences = preferences_service
        self._integration = integration
        self._memory_cache = memory_cache
        self._wikidata_repo = wikidata_repo
        self._lfm_repo = lastfm_repo
        self._enrich_in_flight: dict[
            tuple[str, int], asyncio.Future[DiscoverQueueEnrichment]
        ] = {}
        # A2 part 3: LB popularity batch coalescer state (single-process).
        self._popularity_pending: dict[str, "asyncio.Future[int | None]"] = {}
        self._popularity_flush_handle: asyncio.TimerHandle | None = None
        self._popularity_flush_task: asyncio.Task | None = None

    async def enrich_queue_item(
        self,
        release_group_mbid: str,
        *,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
    ) -> DiscoverQueueEnrichment:
        """A2 part 1: ``priority`` defaults to BACKGROUND_SYNC - queue
        hydration is background composition and must neither occupy user
        slots nor re-mark user activity (which self-starved its own legs)."""
        source_context = capture_mb_source_context()
        release_group_mbid = normalize_mb_id(release_group_mbid)
        cache_key = f"{DISCOVER_QUEUE_ENRICH_PREFIX}{release_group_mbid}"
        if self._memory_cache:
            cached = await self._memory_cache.get(cache_key)
            if (
                is_mb_source_current(source_context)
                and cached is not None
                and isinstance(cached, DiscoverQueueEnrichment)
            ):
                return cached
            if not is_mb_source_current(source_context):
                source_context = capture_mb_source_context()

        inflight_key = (release_group_mbid, source_context.generation)
        if inflight_key in self._enrich_in_flight:
            return await asyncio.shield(self._enrich_in_flight[inflight_key])

        loop = asyncio.get_running_loop()
        future: asyncio.Future[DiscoverQueueEnrichment] = loop.create_future()
        self._enrich_in_flight[inflight_key] = future
        try:
            result = await self._do_enrich_queue_item(
                release_group_mbid,
                cache_key,
                priority=priority,
                source_context=source_context,
            )
            if not future.done():
                future.set_result(result)
            return result
        except BaseException as exc:
            if not future.done():
                future.set_exception(exc)
            raise
        finally:
            self._enrich_in_flight.pop(inflight_key, None)

    async def _do_enrich_queue_item(
        self,
        release_group_mbid: str,
        cache_key: str,
        *,
        priority: RequestPriority = RequestPriority.BACKGROUND_SYNC,
        source_context: MbSourceContext | None = None,
    ) -> DiscoverQueueEnrichment:
        source_context = source_context or capture_mb_source_context()
        enrichment = DiscoverQueueEnrichment()

        rg_data = await self._mb_repo.get_release_group_by_id(
            release_group_mbid,
            includes=["artist-credits", "releases", "tags", "url-rels"],
            priority=priority,
        )

        artist_mbid = ""
        youtube_url = None
        first_release_id: str | None = None

        if rg_data:
            tags_raw = rg_data.get("tags", [])
            enrichment.tags = [t.get("name", "") for t in tags_raw if t.get("name")][
                :10
            ]

            youtube_raw = self._mb_repo.extract_youtube_url_from_relations(rg_data)
            if youtube_raw:
                youtube_url = self._mb_repo.youtube_url_to_embed(youtube_raw)

            ac_list = rg_data.get("artist-credit", [])
            for ac in ac_list:
                a = ac.get("artist", {}) if isinstance(ac, dict) else {}
                if a.get("id"):
                    artist_mbid = a["id"]
                    break
            enrichment.artist_mbid = artist_mbid or None

            releases = rg_data.get("releases") or rg_data.get("release-list", [])
            if releases:
                first_release = releases[0]
                enrichment.release_date = first_release.get("date")
                first_release_id = first_release.get("id")

        album_name = rg_data.get("title", "") if rg_data else ""
        artist_name_for_search = ""
        if rg_data:
            ac_list = rg_data.get("artist-credit", [])
            for ac in ac_list:
                a = ac.get("artist", {}) if isinstance(ac, dict) else {}
                if a.get("name"):
                    artist_name_for_search = a["name"]
                    break

        async def _hunt_youtube() -> str | None:
            """A2 part 2: release -> <=3 recordings YouTube hunt. Runs
            concurrently with the enrichment legs instead of stacking after
            them."""
            if not first_release_id or youtube_url:
                return None
            release_data = await self._mb_repo.get_release_by_id(
                first_release_id,
                includes=["recordings", "url-rels"],
                priority=priority,
            )
            if not release_data:
                return None
            yt_raw = self._mb_repo.extract_youtube_url_from_relations(release_data)
            if yt_raw:
                return self._mb_repo.youtube_url_to_embed(yt_raw)

            tracks = release_data.get("media") or release_data.get("medium-list", [])
            rec_ids: list[str] = []
            for medium in tracks:
                for track in medium.get("tracks") or medium.get("track-list", []):
                    rec_id = track.get("recording", {}).get("id")
                    if rec_id:
                        rec_ids.append(rec_id)
                    if len(rec_ids) >= 3:
                        break
                if len(rec_ids) >= 3:
                    break
            if not rec_ids:
                return None
            rec_results = await asyncio.gather(
                *[
                    self._mb_repo.get_recording_by_id(rid, includes=["url-rels"])
                    for rid in rec_ids
                ],
                return_exceptions=True,
            )
            for rec_data in rec_results:
                if isinstance(rec_data, Exception) or not rec_data:
                    continue
                yt_raw = self._mb_repo.extract_youtube_url_from_relations(rec_data)
                if yt_raw:
                    return self._mb_repo.youtube_url_to_embed(yt_raw)
            return None

        async def _get_artist_and_bio():
            if not artist_mbid:
                return
            try:
                mb_artist = await self._mb_repo.get_artist_by_id(
                    artist_mbid, priority=priority
                )
                if mb_artist:
                    enrichment.country = mb_artist.get("country") or mb_artist.get(
                        "area", {}
                    ).get("name", "")
                    if self._wikidata_repo:
                        url_rels = mb_artist.get("relations", [])
                        wiki_url = None
                        for rel in url_rels:
                            if rel.get("type") in ("wikipedia", "wikidata"):
                                url_obj = rel.get("url", {})
                                wiki_url = (
                                    url_obj.get("resource", "")
                                    if isinstance(url_obj, dict)
                                    else ""
                                )
                                break
                        if wiki_url:
                            bio = await self._wikidata_repo.get_wikipedia_extract(
                                wiki_url
                            )
                            if bio:
                                enrichment.artist_description = bio
            except Exception as e:  # noqa: BLE001
                logger.debug(f"Failed to get artist MB data: {e}")

        async def _get_listen_count():
            try:
                # A2 part 3: route through the windowed batch coalescer so a
                # whole build collapses into one or two LB POSTs. The repo
                # method itself is per-MBID cached + deduped (B4).
                count = await asyncio.shield(
                    self._coalesce_popularity(release_group_mbid)
                )
                if count is not None:
                    enrichment.listen_count = count
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001
                logger.debug(f"Failed to get listen count: {e}")

        async def _apply_lastfm_fallback():
            if not self._lfm_repo or not self._integration.is_lastfm_enabled():
                return
            if not album_name or not artist_name_for_search:
                return

            try:
                album_info = await self._lfm_repo.get_album_info(
                    artist=artist_name_for_search,
                    album=album_name,
                )
                if album_info:
                    if not enrichment.tags and album_info.tags:
                        enrichment.tags = [
                            tag.name for tag in album_info.tags if tag.name
                        ][:10]
                    if not enrichment.artist_description and album_info.summary:
                        cleaned_summary = clean_lastfm_bio(album_info.summary)
                        if cleaned_summary:
                            enrichment.artist_description = cleaned_summary
            except Exception as e:  # noqa: BLE001
                logger.debug("Failed Last.fm album fallback for discover queue: %s", e)

            if enrichment.artist_description and enrichment.tags:
                return

            try:
                artist_info = await self._lfm_repo.get_artist_info(
                    artist=artist_name_for_search,
                    mbid=artist_mbid or None,
                )
                if not artist_info:
                    return
                if not enrichment.artist_mbid and artist_info.mbid:
                    enrichment.artist_mbid = artist_info.mbid
                if not enrichment.tags and artist_info.tags:
                    enrichment.tags = [
                        tag.name for tag in artist_info.tags if tag.name
                    ][:10]
                if not enrichment.artist_description and artist_info.bio_summary:
                    cleaned_bio = clean_lastfm_bio(artist_info.bio_summary)
                    if cleaned_bio:
                        enrichment.artist_description = cleaned_bio
            except Exception as e:  # noqa: BLE001
                logger.debug("Failed Last.fm artist fallback for discover queue: %s", e)

        async def _bio_then_lastfm():
            # A2 part 2 deviation (documented): the Last.fm fallback keeps its
            # fill-the-gaps precedence by sequencing AFTER the wiki bio, while
            # still overlapping the MB-artist RTT with the YouTube hunt and
            # the LB POST. Guarantees order-stable results under overlap.
            await _get_artist_and_bio()
            await _apply_lastfm_fallback()

        # A2 part 2: overlap the independent workstreams. Every task writes
        # disjoint fields (except the sequenced bio->lastfm pair above), and
        # the finally-cancel below leaves no orphan futures.
        tasks: list[asyncio.Task[Any]] = [
            asyncio.create_task(_hunt_youtube()),
            asyncio.create_task(_get_listen_count()),
            asyncio.create_task(_bio_then_lastfm()),
        ]
        try:
            results = await asyncio.gather(*tasks)
            hunted = results[0]
            if hunted:
                youtube_url = hunted
        finally:
            for task in tasks:
                if not task.done():
                    task.cancel()

        if not youtube_url:
            yt_settings = self._preferences.get_youtube_connection()
            enrichment.youtube_search_available = (
                yt_settings.enabled
                and yt_settings.api_enabled
                and yt_settings.has_valid_api_key()
            )

        enrichment.youtube_url = youtube_url
        enrichment.youtube_search_url = f"https://www.youtube.com/results?search_query={quote_plus(f'{artist_name_for_search} {album_name}')}"

        if self._memory_cache:
            enrich_ttl = self._integration.get_queue_settings().enrich_ttl
            await mb_publish_if_current(
                source_context,
                lambda: self._memory_cache.set(cache_key, enrichment, enrich_ttl),
            )

        return enrichment

    # A2 part 3: LB popularity batch coalescer.
    #
    # Collapses concurrent 1-item popularity calls within one build into a
    # single get_release_group_popularity_batch POST (window OR size cap,
    # whichever fires first). Caching/dedup lives downstream in the repo
    # method (B4 landed lb_rg_popularity keys + _metadata_deduplicator) -
    # this layer only batches, never caches.

    _POPULARITY_WINDOW_SECONDS = 0.5
    _POPULARITY_MAX_BATCH = 50

    def _enqueue_popularity(self, mbid: str) -> "asyncio.Future[int | None]":
        loop = asyncio.get_running_loop()
        existing = self._popularity_pending.get(mbid)
        if existing is not None:
            return existing

        future: asyncio.Future[int | None] = loop.create_future()
        self._popularity_pending[mbid] = future

        if len(self._popularity_pending) >= self._POPULARITY_MAX_BATCH:
            # Size cap reached: flush immediately (no timer needed).
            self._flush_popularity_now()
        elif self._popularity_flush_handle is None:
            self._popularity_flush_handle = loop.call_later(
                self._POPULARITY_WINDOW_SECONDS, self._flush_popularity_now
            )
        return future

    def _flush_popularity_now(self) -> None:
        handle = self._popularity_flush_handle
        self._popularity_flush_handle = None
        if handle is not None:
            handle.cancel()
        pending = self._popularity_pending
        self._popularity_pending = {}
        if pending:
            self._popularity_flush_task = asyncio.create_task(
                self._deliver_popularity(pending)
            )

    async def _deliver_popularity(
        self, pending: dict[str, "asyncio.Future[int | None]"]
    ) -> None:
        mbids = sorted(pending)
        try:
            counts = await self._lb_repo.get_release_group_popularity_batch(mbids)
        except asyncio.CancelledError:
            # Window task cancelled mid-flight: fail every waiter explicitly
            # so nobody awaits a future that will never resolve.
            for future in pending.values():
                if not future.done():
                    future.set_exception(asyncio.CancelledError())
            raise
        except Exception as exc:  # noqa: BLE001 - fanned out to every waiter below
            # Leader exception fans out to ALL waiters before returning.
            for future in pending.values():
                if not future.done():
                    future.set_exception(exc)
            logger.debug(
                "Popularity coalescer flush failed (%s mbids): %s",
                len(mbids),
                exc,
            )
            return

        counts = counts if isinstance(counts, dict) else {}
        for mbid, future in pending.items():
            if not future.done():
                future.set_result(counts.get(mbid))

    async def _coalesce_popularity(self, mbid: str) -> int | None:
        future = self._enqueue_popularity(mbid)
        # Shield: this waiter's cancellation must not poison the shared future.
        return await asyncio.shield(future)
