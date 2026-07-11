"""Track-level radio plans for hands-free smart playback.

The backend returns a *plan* (ordered desired tracks with library membership);
the frontend resolves playback: library tracks stream natively via the user's
playback sources, un-owned tracks resolve lazily to YouTube or 30s previews.

Time-to-first-sound budget is <=5s: the ``fast`` first call expands only the
seed itself plus the local library pool (one LB call at most); the frontend
immediately asks for the full plan in the background.
"""

import asyncio
import hashlib
import logging
import random
from datetime import datetime, timezone
from typing import Any

from api.v1.schemas.discover import RadioPlanRequest, RadioPlanResponse, RadioPlanTrack
from core.exceptions import ValidationError
from repositories.protocols import (
    LastFmRepositoryProtocol,
    ListenBrainzRepositoryProtocol,
    MusicBrainzRepositoryProtocol,
)
from services.discover.mbid_resolution_service import MbidResolutionService

logger = logging.getLogger(__name__)

VARIOUS_ARTISTS_MBID = "89ad4ac3-39f7-470e-963a-56509c546377"

_MAX_SEED_ARTISTS = 8
_FAST_SEED_ARTISTS = 2
_TRACKS_PER_ARTIST_EXTERNAL = 5
_MAX_PER_ARTIST = 4
_MAX_CONSECUTIVE_SAME_ARTIST = 2
# library-first pool depth: caps below keep the IN(...) placeholder count safe
# (mbids are bound twice in get_files_by_artist_mbids; SQLite default cap is 999)
_MAX_POOL_ARTISTS = 300
_ADJACENT_GENRES = 6


class RadioPlanService:
    def __init__(
        self,
        lb_repo: ListenBrainzRepositoryProtocol,
        mb_repo: MusicBrainzRepositoryProtocol,
        mbid_svc: MbidResolutionService,
        library_db: Any = None,
        genre_index: Any = None,
        lfm_repo: LastFmRepositoryProtocol | None = None,
        preview_repo: Any = None,
    ) -> None:
        self._lb_repo = lb_repo
        self._mb_repo = mb_repo
        self._mbid = mbid_svc
        self._library_db = library_db
        self._genre_index = genre_index
        self._lfm_repo = lfm_repo
        self._preview_repo = preview_repo

    async def build_plan(
        self, user_id: str, request: RadioPlanRequest, *, max_count: int = 50
    ) -> RadioPlanResponse:
        # max_count lets non-radio callers (Smart Mix saved playlists) ask for a
        # deeper plan than the live-radio default cap of 50
        seeds, title = await self._expand_seeds(user_id, request)
        # library-mode genre stations can still fill from file-tag genre matches
        # even when the genre index yields no seed artists - don't bail early
        library_genre_station = request.mode == "library" and request.seed_type == "genre"
        if not seeds and not library_genre_station:
            return RadioPlanResponse(title=title, tracks=[])

        exclude_recordings = {m.lower() for m in request.exclude_recording_mbids}
        count = max(5, min(request.count, max_count))

        library_pool = await self._library_tracks(seeds, exclude_recordings)
        external_pool: list[RadioPlanTrack] = []
        if request.mode == "hybrid":
            external_pool = await self._external_tracks(seeds, exclude_recordings)
        elif len(library_pool) < count:
            # library-first: the seed pool alone is thin (a genre station samples
            # only a few index artists) - deepen it from the user's own files.
            # Pure library-DB work: fast, offline, no external calls.
            library_pool = await self._library_first_pool(
                request, seeds, library_pool, exclude_recordings, count
            )

        tracks = self._mix(library_pool, external_pool, count, request.mode)
        return RadioPlanResponse(title=title, tracks=tracks)

    async def _expand_seeds(
        self, user_id: str, request: RadioPlanRequest
    ) -> tuple[list[tuple[str, str]], str]:
        """Resolve the request into a list of (artist_mbid, artist_name) seeds."""
        seed_cap = _FAST_SEED_ARTISTS if request.fast else _MAX_SEED_ARTISTS

        if request.seed_type == "items":
            seen: set[str] = set()
            seeds: list[tuple[str, str]] = []
            for item in request.items:
                mbid = self._mbid.normalize_mbid(item.artist_mbid)
                if not mbid or mbid in seen or mbid == VARIOUS_ARTISTS_MBID:
                    continue
                seen.add(mbid)
                seeds.append((mbid, item.artist_name))
            return seeds[:_MAX_SEED_ARTISTS], "Radio: This Shelf"

        if not request.seed_id or not request.seed_id.strip():
            raise ValidationError("seed_id must be non-empty")

        if request.seed_type == "genre":
            return await self._expand_genre_seeds(user_id, request.seed_id, seed_cap)

        seed_mbid = self._mbid.normalize_mbid(request.seed_id)
        if not seed_mbid:
            raise ValidationError(f"Unknown seed MBID: {request.seed_id}")

        if request.seed_type == "album":
            rg = await self._mb_repo.get_release_group(seed_mbid)
            if not rg:
                return [], "Radio"
            artist_mbid = self._mbid.normalize_mbid(rg.artist_id) or ""
            title = f"Radio: {rg.title}"
            base = [(artist_mbid, getattr(rg, "artist_name", "") or "")] if artist_mbid else []
        else:  # artist
            name = seed_mbid
            try:
                rgs = await self._lb_repo.get_artist_top_release_groups(seed_mbid, count=1)
                if rgs:
                    name = rgs[0].artist_name or seed_mbid
            except Exception:  # noqa: BLE001
                pass
            title = f"Radio: {name}"
            base = [(seed_mbid, name)]

        if not base:
            return [], title
        seeds = list(base)
        if not request.fast:
            seeds.extend(await self._similar_artists(base[0][0], seed_cap - 1))
        return seeds[:seed_cap], title

    async def _expand_genre_seeds(
        self, user_id: str, genre: str, seed_cap: int
    ) -> tuple[list[tuple[str, str]], str]:
        title = f"Radio: {genre.title()}"
        seeds: list[tuple[str, str]] = []
        seen: set[str] = set()

        if self._genre_index is not None:
            try:
                by_genre = await self._genre_index.get_artists_for_genres([genre])
                mbids = by_genre.get(genre.strip().lower(), [])
                # deterministic per day so refreshes within a day reproduce the station
                seed_hash = int(
                    hashlib.md5(
                        f"{user_id}:{genre}:{datetime.now(timezone.utc).date().isoformat()}".encode()
                    ).hexdigest()[:8],
                    16,
                )
                rng = random.Random(seed_hash)
                sample = rng.sample(mbids, min(len(mbids), max(2, seed_cap // 2)))
                for mbid in sample:
                    norm = self._mbid.normalize_mbid(mbid)
                    if norm and norm not in seen:
                        seen.add(norm)
                        seeds.append((norm, ""))
            except Exception as e:  # noqa: BLE001
                logger.debug("Genre index seed expansion failed for %s: %s", genre, e)

        # external fallback/top-up: Last.fm tag artists (works for non-library genres)
        if len(seeds) < seed_cap and self._lfm_repo is not None:
            try:
                tag_artists = await self._lfm_repo.get_tag_top_artists(genre, limit=10)
                for artist in tag_artists:
                    if len(seeds) >= seed_cap:
                        break
                    mbid = self._mbid.normalize_mbid(artist.mbid)
                    if mbid and mbid not in seen and mbid != VARIOUS_ARTISTS_MBID:
                        seen.add(mbid)
                        seeds.append((mbid, artist.name))
            except Exception as e:  # noqa: BLE001
                logger.debug("Last.fm tag seed expansion failed for %s: %s", genre, e)

        return seeds[:seed_cap], title

    async def _similar_artists(self, seed_mbid: str, limit: int) -> list[tuple[str, str]]:
        try:
            similar = await self._lb_repo.get_similar_artists(seed_mbid, max_similar=limit + 2)
        except Exception as e:  # noqa: BLE001
            logger.debug("Similar-artist expansion failed for %s: %s", seed_mbid[:8], e)
            return []
        out: list[tuple[str, str]] = []
        for artist in similar:
            mbid = self._mbid.normalize_mbid(artist.artist_mbid)
            if not mbid or mbid == VARIOUS_ARTISTS_MBID:
                continue
            out.append((mbid, artist.artist_name))
            if len(out) >= limit:
                break
        return out

    async def _library_tracks(
        self, seeds: list[tuple[str, str]], exclude_recordings: set[str]
    ) -> list[RadioPlanTrack]:
        if self._library_db is None:
            return []
        try:
            rows = await self._library_db.get_files_by_artist_mbids(
                [mbid for mbid, _ in seeds], limit=120
            )
        except Exception as e:  # noqa: BLE001
            logger.debug("Library radio pool failed: %s", e)
            return []
        return self._rows_to_plan_tracks(rows, exclude_recordings, set())

    @staticmethod
    def _rows_to_plan_tracks(
        rows: list[dict[str, Any]],
        exclude_recordings: set[str],
        seen_titles: set[str],
    ) -> list[RadioPlanTrack]:
        """Convert library_files rows into plan tracks, deduped by artist|title.

        ``seen_titles`` is shared across pools so successive fills never re-add
        a track already claimed by an earlier pool.
        """
        tracks: list[RadioPlanTrack] = []
        for row in rows:
            recording = (row.get("recording_mbid") or "").lower()
            if recording and recording in exclude_recordings:
                continue
            title_key = f"{(row.get('artist_name') or '').lower()}|{(row.get('track_title') or '').lower()}"
            if title_key in seen_titles:
                continue
            seen_titles.add(title_key)
            tracks.append(RadioPlanTrack(
                track_name=row.get("track_title") or "Unknown",
                artist_name=row.get("artist_name") or row.get("album_artist_name") or "Unknown",
                artist_mbid=row.get("artist_mbid") or row.get("album_artist_mbid") or "",
                recording_mbid=row.get("recording_mbid"),
                album_mbid=row.get("release_group_mbid"),
                album_name=row.get("album_title"),
                in_library=True,
                local_file_id=row.get("id"),
                file_format=row.get("file_format"),
                duration_s=row.get("duration_seconds"),
            ))
        return tracks

    async def _library_first_pool(
        self,
        request: RadioPlanRequest,
        seeds: list[tuple[str, str]],
        base_pool: list[RadioPlanTrack],
        exclude_recordings: set[str],
        count: int,
    ) -> list[RadioPlanTrack]:
        """Deepen a thin library pool from the user's own files (library mode).

        Genre stations pull EVERY library artist tagged with the genre plus
        file-tag genre matches; artist/album/items stations pull artists
        adjacent to the seeds through shared library genres. Everything here is
        local SQLite - no external calls, so it also runs on ``fast`` plans.
        Seed-artist tracks stay at the head; the widened pool is shuffled so the
        _mix diversity pass has varied material.
        """
        if self._library_db is None:
            return base_pool
        seen_titles = {f"{t.artist_name.lower()}|{t.track_name.lower()}" for t in base_pool}
        extra: list[RadioPlanTrack] = []
        fetch_limit = max(count * 6, 200)
        genre = request.seed_id if request.seed_type == "genre" else None
        seed_mbids = {mbid for mbid, _ in seeds if mbid}

        pool_artist_mbids: list[str] = []
        if self._genre_index is not None:
            try:
                if genre:
                    by_genre = await self._genre_index.get_artists_for_genres([genre])
                    pool_artist_mbids = list(by_genre.get(genre.strip().lower(), []))
                elif seed_mbids:
                    genres_by_artist = await self._genre_index.get_genres_for_artists(
                        sorted(seed_mbids)
                    )
                    adjacent_genres: list[str] = []
                    seen_genres: set[str] = set()
                    for artist_genres in genres_by_artist.values():
                        for g in artist_genres:
                            g_lower = g.strip().lower()
                            if g_lower and g_lower not in seen_genres:
                                seen_genres.add(g_lower)
                                adjacent_genres.append(g)
                    if adjacent_genres:
                        by_genre = await self._genre_index.get_artists_for_genres(
                            adjacent_genres[:_ADJACENT_GENRES]
                        )
                        seen_mbids: set[str] = set()
                        for mbids in by_genre.values():
                            for mbid in mbids:
                                if mbid not in seen_mbids:
                                    seen_mbids.add(mbid)
                                    pool_artist_mbids.append(mbid)
            except Exception as e:  # noqa: BLE001
                logger.debug("Library-first adjacency lookup failed: %s", e)

        pool_artist_mbids = [m for m in pool_artist_mbids if m not in seed_mbids]
        if pool_artist_mbids:
            try:
                rows = await self._library_db.get_files_by_artist_mbids(
                    pool_artist_mbids[:_MAX_POOL_ARTISTS], limit=fetch_limit
                )
                extra.extend(self._rows_to_plan_tracks(rows, exclude_recordings, seen_titles))
            except Exception as e:  # noqa: BLE001
                logger.debug("Library-first artist pool failed: %s", e)

        # file-tag genre matches catch tracks whose artists never made the index
        if genre:
            try:
                rows = await self._library_db.get_files_by_genre(genre, limit=fetch_limit)
                extra.extend(self._rows_to_plan_tracks(rows, exclude_recordings, seen_titles))
            except Exception as e:  # noqa: BLE001
                logger.debug("Library-first genre pool failed: %s", e)

        random.shuffle(extra)
        return base_pool + extra

    async def _artist_top_tracks(
        self, artist_mbid: str, artist_name: str
    ) -> list[tuple[str, str, str | None]]:
        """(track, artist, recording_mbid) per source, falling through the chain:
        ListenBrainz popularity -> Last.fm -> Deezer. LB's popularity API goes down
        under load (observed live 2026-07-03), so radio must not depend on it."""
        try:
            recordings = await self._lb_repo.get_artist_top_recordings(
                artist_mbid, count=_TRACKS_PER_ARTIST_EXTERNAL
            )
            if recordings:
                return [
                    (rec.track_name, rec.artist_name or artist_name, rec.recording_mbid)
                    for rec in recordings
                ]
        except Exception as e:  # noqa: BLE001 - fall through the chain
            logger.debug("LB top recordings failed for %s: %s", artist_mbid[:8], e)

        if artist_name and self._lfm_repo is not None:
            try:
                lfm_tracks = await self._lfm_repo.get_artist_top_tracks(
                    artist_name, mbid=artist_mbid, limit=_TRACKS_PER_ARTIST_EXTERNAL
                )
                if lfm_tracks:
                    return [(t.name, artist_name, t.mbid or None) for t in lfm_tracks]
            except Exception as e:  # noqa: BLE001 - fall through the chain
                logger.debug("Last.fm top tracks failed for %s: %s", artist_name, e)

        if artist_name and self._preview_repo is not None:
            try:
                deezer_tracks = await self._preview_repo.get_artist_top_tracks(
                    artist_name, limit=_TRACKS_PER_ARTIST_EXTERNAL
                )
                if deezer_tracks:
                    return [(t.title, t.artist_name or artist_name, None) for t in deezer_tracks]
            except Exception as e:  # noqa: BLE001 - end of the chain
                logger.debug("Deezer top tracks failed for %s: %s", artist_name, e)
        return []

    async def _external_tracks(
        self, seeds: list[tuple[str, str]], exclude_recordings: set[str]
    ) -> list[RadioPlanTrack]:
        results = await asyncio.gather(
            *(self._artist_top_tracks(mbid, name) for mbid, name in seeds),
            return_exceptions=True,
        )
        tracks: list[RadioPlanTrack] = []
        seen: set[str] = set()
        for (artist_mbid, artist_name), rows in zip(seeds, results):
            if isinstance(rows, Exception):
                continue
            for track_name, track_artist, recording_mbid in rows:
                rec_lower = (recording_mbid or "").lower()
                if rec_lower and rec_lower in exclude_recordings:
                    continue
                key = f"{track_artist.lower()}|{track_name.lower()}"
                if key in seen:
                    continue
                seen.add(key)
                tracks.append(RadioPlanTrack(
                    track_name=track_name,
                    artist_name=track_artist or artist_name,
                    artist_mbid=artist_mbid,
                    recording_mbid=recording_mbid,
                    in_library=False,
                ))
        return tracks

    @staticmethod
    def _mix(
        library_pool: list[RadioPlanTrack],
        external_pool: list[RadioPlanTrack],
        count: int,
        mode: str,
    ) -> list[RadioPlanTrack]:
        """Interleave pools targeting ~50/50 in hybrid, with per-artist caps and
        no more than two consecutive tracks by the same artist."""
        pools = [library_pool] if mode == "library" else [library_pool, external_pool]
        pools = [list(p) for p in pools if p]
        if not pools:
            return []

        selected: list[RadioPlanTrack] = []
        seen_keys: set[str] = set()
        per_artist: dict[str, int] = {}
        indices = [0] * len(pools)

        def artist_key(t: RadioPlanTrack) -> str:
            return t.artist_mbid.lower() or t.artist_name.lower()

        stall = 0
        pool_i = 0
        while len(selected) < count and stall < sum(len(p) for p in pools) + len(pools):
            pool = pools[pool_i % len(pools)]
            idx = indices[pool_i % len(pools)]
            pool_i += 1
            if idx >= len(pool):
                stall += 1
                continue
            indices[(pool_i - 1) % len(pools)] = idx + 1
            track = pool[idx]
            key = f"{track.artist_name.lower()}|{track.track_name.lower()}"
            a_key = artist_key(track)
            if key in seen_keys:
                stall += 1
                continue
            if per_artist.get(a_key, 0) >= _MAX_PER_ARTIST:
                stall += 1
                continue
            recent = [artist_key(t) for t in selected[-_MAX_CONSECUTIVE_SAME_ARTIST:]]
            if len(recent) == _MAX_CONSECUTIVE_SAME_ARTIST and all(r == a_key for r in recent):
                stall += 1
                continue
            seen_keys.add(key)
            per_artist[a_key] = per_artist.get(a_key, 0) + 1
            selected.append(track)
            stall = 0

        if mode == "library" and len(selected) < count:
            # relaxed fill (library mode only - hybrid stays as before): lift
            # the per-artist cap (a library with few artists in a genre must
            # still fill the station) but KEEP the consecutive-same-artist rule
            # so diversity survives
            remaining = [
                t
                for pool in pools
                for t in pool
                if f"{t.artist_name.lower()}|{t.track_name.lower()}" not in seen_keys
            ]
            progress = True
            while len(selected) < count and remaining and progress:
                progress = False
                for i, track in enumerate(remaining):
                    key = f"{track.artist_name.lower()}|{track.track_name.lower()}"
                    if key in seen_keys:
                        remaining.pop(i)
                        progress = True
                        break
                    a_key = artist_key(track)
                    recent = [artist_key(t) for t in selected[-_MAX_CONSECUTIVE_SAME_ARTIST:]]
                    if len(recent) == _MAX_CONSECUTIVE_SAME_ARTIST and all(
                        r == a_key for r in recent
                    ):
                        continue
                    seen_keys.add(key)
                    selected.append(track)
                    remaining.pop(i)
                    progress = True
                    break
        return selected
