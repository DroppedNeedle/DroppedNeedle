"""Follow new-release detection and policy-aware auto-download fan-out.

For each artist, the transition lock covers the preference snapshot through
persistence and dispatch, so one artist decision cannot split across release-type
revisions. Provider I/O remains outside the lock, so a multi-artist run may
straddle a settings save by design.

A first poll, or the first poll after a policy revision change, records every
observed release-group as a no-feed/no-task baseline. Normal polls only emit
complete, valid release dates on or after the prior successful UTC cursor date.
Future matching releases remain durable dispatch-pending until their date.
"""

import asyncio
import logging
import re
import json
import time
import uuid
from collections.abc import Callable
from datetime import date, datetime, timezone

import msgspec

from core.exceptions import ConfigurationError
from infrastructure.persistence.follow_store import (
    DistinctFollowedArtist,
    FollowStore,
    InventoryInvalidated,
    NewReleaseInput,
)
from infrastructure.queue.priority_queue import RequestPriority
from models.release_type_policy import should_include_release
from services.native.download_service import ALREADY_IN_LIBRARY
from repositories.musicbrainz_base import MbCachePolicy, capture_mb_source_context, mb_publish_if_current
from infrastructure.observability.provider_counters import ProviderWorkload, provider_workload

logger = logging.getLogger(__name__)

_MB_PAGE_LIMIT = 100
_PROCESS = uuid.uuid4().hex
_MB_FETCH_TIMEOUT = 30.0
_COMPLETE_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


class PollSummary(msgspec.Struct, frozen=True):
    artists_polled: int = 0
    baselined: int = 0
    new_releases: int = 0
    enqueued: int = 0
    errors: int = 0


class _ArtistPollResult(msgspec.Struct, frozen=True):
    baselined: bool = False
    new_releases: int = 0
    enqueued: int = 0


class _EnqueueResult(msgspec.Struct, frozen=True):
    enqueued: bool = False
    terminal: bool = False


def _parse_release_date(value: str | None) -> date | None:
    """Parse only a complete MusicBrainz calendar date."""
    if not isinstance(value, str):
        return None
    value = value.strip()
    if not _COMPLETE_DATE_RE.fullmatch(value):
        return None
    try:
        return date.fromisoformat(value)
    except ValueError:
        return None


def _utc_cursor_date(timestamp: float | None) -> date | None:
    if timestamp is None:
        return None
    try:
        return datetime.fromtimestamp(float(timestamp), timezone.utc).date()
    except (OverflowError, OSError, TypeError, ValueError):
        return None


class NewReleaseService:
    def __init__(
        self,
        follow_store: FollowStore,
        mb_repo,
        acquisition,
        download_store,
        library_repo,
        sse_publisher,
        inter_artist_delay: float = 1.1,
        *,
        preferences_service,
        policy_transition_lock: asyncio.Lock,
        today_factory: Callable[[], date] = date.today,
    ) -> None:
        self._store = follow_store
        self._mb = mb_repo
        # The dispatcher routes to a user's download client or Free Music; it resolves
        # both fresh per call, so capturing it in this background loop is safe.
        self._acquisition = acquisition
        self._download_store = download_store
        self._library = library_repo
        self._sse = sse_publisher
        self._inter_artist_delay = inter_artist_delay
        self._preferences = preferences_service
        self._policy_transition_lock = policy_transition_lock
        self._today_factory = today_factory
        self._poll_lock = asyncio.Lock()

    async def run_poll(self) -> PollSummary:
        async with self._poll_lock:
            with provider_workload(ProviderWorkload.FOLLOW):
                return await self._run_due_poll()

    async def _run_due_poll(self) -> PollSummary:
        attempted = 0
        visited: set[str] = set()
        baselined = new_releases = enqueued = errors = 0
        while attempted < 10:
            artists = await self._store.list_due_artists(time.time(), 10)
            if not artists:
                break
            for artist in artists:
                attempted += 1
                visited.add(artist.artist_mbid_lower)
                try:
                    result = await self._process_artist(artist)
                    baselined += int(result.baselined)
                    new_releases += result.new_releases
                    enqueued += result.enqueued
                except InventoryInvalidated:
                    pass
                except Exception as exc:  # noqa: BLE001 - isolate each artist
                    logger.warning("Follow poll failed for %s: %s", artist.artist_mbid_lower, exc)
                    errors += 1
                if attempted >= 10:
                    break
                if self._inter_artist_delay > 0:
                    await asyncio.sleep(self._inter_artist_delay)
        return PollSummary(
            artists_polled=len(visited), baselined=baselined,
            new_releases=new_releases, enqueued=enqueued, errors=errors,
        )

    async def _owned_release_groups(self) -> set[str]:
        try:
            owned = await self._library.get_library_mbids(include_release_ids=False)
        except Exception as exc:  # noqa: BLE001 - degrade, do not crash the run
            logger.warning("Follow poll: could not load owned release groups: %s", exc)
            return set()
        return {str(m).casefold() for m in owned}

    async def _process_artist(self, artist: DistinctFollowedArtist) -> _ArtistPollResult:
        context = capture_mb_source_context()
        source = json.dumps([context.source_mode, context.source_id, context.generation])
        async with self._policy_transition_lock:
            _, revision = self._preferences.get_preferences_with_revision()
            state = None
            async def prepare():
                nonlocal state
                state = await self._store.prepare_inventory(artist.artist_mbid_lower, source, revision, _PROCESS)
            if not await mb_publish_if_current(context, prepare):
                return _ArtistPollResult()
        try:
            return await self._observe_artist(artist, context, source, revision, state)
        except Exception as exc:
            async with self._policy_transition_lock:
                _, current_revision = self._preferences.get_preferences_with_revision()
                if current_revision != revision:
                    raise InventoryInvalidated() from exc
                recorded = False
                async def fail():
                    nonlocal recorded
                    snapshot = state or {}
                    # A failed mid-walk observation must not re-walk within the
                    # hour; fresh observations keep the ladder (a pure 429 rides
                    # retry_after through max() with no floor).
                    walk_failure = (
                        snapshot.get("phase") == "verifying"
                        or (snapshot.get("offset") or 0) > 0
                        or (snapshot.get("diverge_count") or 0) > 0
                    )
                    recorded = await self._store.fail_inventory(
                        state,
                        str(exc),
                        float(getattr(exc, "retry_after_seconds", 0) or 0),
                        min_delay=3600 if walk_failure else 0,
                    )
                if not await mb_publish_if_current(context, fail) or not recorded:
                    raise InventoryInvalidated() from exc
            raise

    async def _observe_artist(self, artist, context, source, revision, state) -> _ArtistPollResult:
        release_groups, total, response_context = await asyncio.wait_for(
            self._mb.get_artist_release_groups_with_context(
                artist.artist_mbid, offset=state["offset"], limit=_MB_PAGE_LIMIT,
                priority=RequestPriority.BACKGROUND_SYNC,
                preserve_fetch_width=True, source_context=context,
                # Detection freshness: the poller must see live pages, never
                # L1-cached ones. Cadence stays bounded by tick jitter plus
                # the 1 h fail-path re-walk floor.
                cache_policy=MbCachePolicy.BYPASS,
            ), timeout=_MB_FETCH_TIMEOUT,
        )
        if response_context != context:
            return _ArtistPollResult()
        if any(not isinstance(row, dict) or not isinstance(row.get("id"), str) or not row["id"] for row in release_groups):
            raise ValueError("Incomplete release-group page")
        rows = [{key: row.get(key) for key in ("id", "title", "first-release-date", "primary-type", "secondary-types")} for row in release_groups]
        async with self._policy_transition_lock:
            preferences, current_revision = self._preferences.get_preferences_with_revision()
            if current_revision != revision:
                return _ArtistPollResult()
            observed_groups = None
            async def stage():
                nonlocal observed_groups
                observed_groups = await self._store.stage_inventory_page(state, rows, total)
            if not await mb_publish_if_current(context, stage) or observed_groups is None:
                return _ArtistPollResult()
            owned = await self._owned_release_groups()
            detection_result = _ArtistPollResult()
            dispatch_candidates = []
            async def detect():
                nonlocal detection_result, dispatch_candidates
                detection_result, dispatch_candidates = await self._detect_inventory(
                    artist, owned, observed_groups, preferences, revision, source,
                )
            if not await mb_publish_if_current(context, detect):
                return _ArtistPollResult()
            enqueued = 0
            for release_group in dispatch_candidates:
                result = await self._enqueue_for_followers(release_group, artist, owned, self._today_factory())
                if result.terminal:
                    await self._store.clear_pending_release(artist.artist_mbid_lower, release_group["id"])
                enqueued += int(result.enqueued)
            return _ArtistPollResult(baselined=detection_result.baselined, new_releases=detection_result.new_releases, enqueued=enqueued)

    async def _detect_inventory(self, artist, owned, observed_groups, preferences, policy_revision, source):
        observed_lowers = [row["id"].casefold() for row in observed_groups]
        state = await self._store.get_release_check_state(artist.artist_mbid_lower)
        if (
            state is None
            or state.release_type_policy_revision is None
            or state.release_type_policy_revision != policy_revision
        ):
            await self._store.seed_baseline(
                artist.artist_mbid_lower,
                observed_lowers,
                policy_revision,
                inventory_source=source,
            )
            return _ArtistPollResult(baselined=True), []

        known = await self._store.known_release_set(artist.artist_mbid_lower)
        pending = await self._store.pending_release_set(
            artist.artist_mbid_lower, policy_revision
        )
        prior_cursor_date = _utc_cursor_date(state.last_checked_at)
        today = self._today_factory()
        candidates = [
            release_group
            for release_group in observed_groups
            if should_include_release(
                release_group,
                preferences.secondary_types,
                preferences.primary_types,
            )
        ]
        candidates_by_id = {
            release_group["id"].casefold(): release_group
            for release_group in candidates
        }

        # A current-revision pending row is already proof that its release
        # passed the historical discovery cutoff. Keep it pending while the
        # observed row remains policy-matching and has a complete date;
        # applying the advancing cursor here would erase failed work.
        preserved_pending: set[str] = set()
        for release_group_id in pending & candidates_by_id.keys():
            release_date = _parse_release_date(
                candidates_by_id[release_group_id].get("first-release-date")
            )
            if release_date is not None:
                preserved_pending.add(release_group_id)

        eligible_fresh: list[dict] = []
        if prior_cursor_date is not None:
            for release_group in candidates:
                release_group_id = release_group["id"].casefold()
                if release_group_id in known or release_group_id in owned:
                    continue
                release_date = _parse_release_date(
                    release_group.get("first-release-date")
                )
                if release_date is not None and release_date >= prior_cursor_date:
                    eligible_fresh.append(release_group)

        eligible_auto_followers: list[str] = []
        if eligible_fresh:
            eligible_auto_followers = (
                await self._store.list_auto_download_followers(
                    artist.artist_mbid_lower
                )
            )

        pending_to_persist = set(preserved_pending)
        if eligible_auto_followers:
            pending_to_persist.update(
                release_group["id"].casefold() for release_group in eligible_fresh
            )
        feed_rows = [
            self._to_input(release_group, artist)
            for release_group in eligible_fresh
        ]
        dispatch_candidates: list[dict] = []
        dispatch_ids: set[str] = set()
        for release_group in eligible_fresh:
            release_group_id = release_group["id"].casefold()
            release_date = _parse_release_date(
                release_group.get("first-release-date")
            )
            if (
                eligible_auto_followers
                and release_date is not None
                and release_date <= today
            ):
                dispatch_candidates.append(release_group)
                dispatch_ids.add(release_group_id)
        for release_group_id in preserved_pending:
            release_group = candidates_by_id[release_group_id]
            release_date = _parse_release_date(
                release_group.get("first-release-date")
            )
            if release_date is not None and release_date <= today:
                if release_group_id not in dispatch_ids:
                    dispatch_candidates.append(release_group)
                    dispatch_ids.add(release_group_id)

        # This transaction is the durable discovery hand-off: each fresh
        # candidate is known/visible in the feed, while only fresh rows with
        # an eligible follower or preserved pending work are marked pending
        # before acquisition starts.
        await self._store.record_new_releases(
            artist.artist_mbid_lower,
            feed_rows,
            [],
            observed_rg_lowers=observed_lowers,
            pending_rg_lowers=sorted(pending_to_persist),
            policy_revision=policy_revision,
            inventory_source=source,
        )

        return _ArtistPollResult(new_releases=len(feed_rows)), dispatch_candidates

    @staticmethod
    def _to_input(rg: dict, artist: DistinctFollowedArtist) -> NewReleaseInput:
        secondary = rg.get("secondary-types") or []
        return NewReleaseInput(
            release_group_mbid=rg["id"],
            release_group_mbid_lower=rg["id"].casefold(),
            artist_mbid_lower=artist.artist_mbid_lower,
            artist_name=artist.artist_name,
            title=rg.get("title") or "",
            primary_type=rg.get("primary-type"),
            secondary_types=",".join(str(value) for value in secondary)
            if secondary
            else None,
            first_release_date=rg.get("first-release-date"),
        )

    async def _enqueue_for_followers(
        self, rg: dict, artist: DistinctFollowedArtist, owned: set[str], today: date
    ) -> _EnqueueResult:
        rg_id = rg["id"]
        rg_lower = rg_id.casefold()
        release_date = _parse_release_date(rg.get("first-release-date"))
        if release_date is None or release_date > today:
            return _EnqueueResult()
        if rg_lower in owned:
            return _EnqueueResult(terminal=True)
        if (
            await self._download_store.get_active_task_for_album_any_user(rg_id)
            is not None
        ):
            return _EnqueueResult(terminal=True)
        followers = await self._store.list_auto_download_followers(
            artist.artist_mbid_lower
        )
        if not followers:
            # Keep feed/known discovery, but no eligible auto owner means this
            # dispatch decision is terminal. It must not be replayed if approval
            # is enabled later.
            return _EnqueueResult(terminal=True)
        owner = followers[0]
        title = rg.get("title") or ""
        try:
            task_id = await self._acquisition.request_album(
                user_id=owner,
                release_group_mbid=rg_id,
                artist_name=artist.artist_name,
                album_title=title,
                artist_mbid=artist.artist_mbid,
                origin="user",
                track_count_priority=RequestPriority.BACKGROUND_SYNC,
            )
        except ConfigurationError:
            logger.info(
                "Follow poll: download client disabled; not auto-downloading %s",
                rg_lower,
            )
            return _EnqueueResult()
        except Exception as exc:  # noqa: BLE001 - one enqueue failure must not abort the artist
            logger.error(
                "Follow poll: failed to enqueue %s: %s", rg_lower, exc, exc_info=True
            )
            return _EnqueueResult()
        if task_id == ALREADY_IN_LIBRARY:
            return _EnqueueResult(terminal=True)
        if not task_id:
            return _EnqueueResult()
        await self._publish_enqueued(owner, artist, rg_id, title, task_id)
        return _EnqueueResult(enqueued=True, terminal=True)

    async def _publish_enqueued(
        self,
        owner: str,
        artist: DistinctFollowedArtist,
        rg_id: str,
        title: str,
        task_id: str,
    ) -> None:
        try:
            await self._sse.publish(
                f"user:{owner}",
                "auto_download_enqueued",
                {
                    "artist_mbid": artist.artist_mbid,
                    "artist_name": artist.artist_name,
                    "release_group_mbid": rg_id,
                    "title": title,
                    "task_id": task_id,
                },
            )
        except Exception as exc:  # noqa: BLE001 - notification is best-effort
            logger.debug("Follow poll: SSE publish failed: %s", exc)
