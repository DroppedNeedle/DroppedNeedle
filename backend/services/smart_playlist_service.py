"""Smart Mix: turn seeds (artists / genres / moods, any blend) into a saved playlist.

Thin orchestration over the radio-plan engine: ``RadioPlanService``'s
library-first pool does the actual track selection (genre-index artists,
file-tag genre matches, shared-genre adjacency, diversity caps). This service
gathers one candidate pool per seed (mood seeds expand to curated genre/tag
families first), blends the pools so each seed gets a roughly equal share of
the final list, and persists the result as a native playlist owned by the
caller.
"""

import asyncio
import logging
from typing import Any

from fastapi import HTTPException

from api.v1.schemas.discover import RadioPlanRequest, RadioPlanTrack
from core.exceptions import ValidationError
from infrastructure.persistence.auth_store import UserRecord
from repositories.playlist_repository import PlaylistRecord, PlaylistTrackRecord
from services.discover.radio_plan_service import RadioPlanService
from services.playlist_service import MAX_NAME_LENGTH, PlaylistService

logger = logging.getLogger(__name__)

DEFAULT_TRACK_COUNT = 25
MAX_TRACK_COUNT = 250
MIN_TRACK_COUNT = 5
MAX_SEEDS = 10
# blend diversity: mirror the radio-plan engine's caps (strict pass), then a
# relaxed fill that lifts the per-artist cap but keeps the consecutive rule
_MAX_PER_ARTIST = 4
_MAX_CONSECUTIVE_SAME_ARTIST = 2
_NAME_SEED_LIMIT = 3

# Curated mood -> genre/tag families, matched against the user's OWN library
# (genre index + file tags). Only tags the library actually contains are used
# as station seeds; a mood with zero matches contributes an empty pool (the
# request only fails when EVERY seed comes back empty).
MOOD_TAG_FAMILIES: dict[str, list[str]] = {
    "chill": ["lo-fi", "chillout", "ambient", "downtempo", "trip hop", "jazz", "bossa nova"],
    "energetic": ["dance", "edm", "electro", "rock", "punk", "hip hop", "drum and bass", "house"],
    "melancholy": ["indie folk", "slowcore", "sadcore", "emo", "singer-songwriter", "blues", "post-rock"],
    "focus": ["ambient", "classical", "instrumental", "post-rock", "idm", "minimal", "piano"],
    "happy": ["pop", "indie pop", "funk", "disco", "soul", "power pop", "ska"],
    "late night": ["neo soul", "r&b", "trip hop", "downtempo", "darkwave", "jazz", "lo-fi"],
    "workout": ["edm", "hip hop", "dance", "electro house", "drum and bass", "metal", "trap"],
    "romantic": ["soul", "r&b", "neo soul", "bossa nova", "dream pop", "soft rock", "jazz"],
}


def _track_key(track: RadioPlanTrack) -> str:
    return f"{track.artist_name.lower()}|{track.track_name.lower()}"


def _artist_key(track: RadioPlanTrack) -> str:
    return track.artist_mbid.lower() or track.artist_name.lower()


class SmartPlaylistService:
    """Generate-and-save playlists from blended seeds via the radio-plan engine."""

    def __init__(
        self,
        radio_plan: RadioPlanService,
        playlist_service: PlaylistService,
        genre_index: Any = None,
        library_db: Any = None,
    ) -> None:
        self._radio_plan = radio_plan
        self._playlist_service = playlist_service
        self._genre_index = genre_index
        self._library_db = library_db

    async def generate(
        self,
        user: UserRecord,
        *,
        seeds: list[tuple[str, str]],
        count: int = DEFAULT_TRACK_COUNT,
        name: str | None = None,
    ) -> tuple[PlaylistRecord, list[PlaylistTrackRecord]]:
        """Blend a library Smart Mix from ``seeds`` ((type, value) pairs, any mix
        of artist/genre/mood) and persist it as a playlist owned by ``user``."""
        cleaned: list[tuple[str, str]] = []
        seen_seeds: set[tuple[str, str]] = set()
        for seed_type, value in seeds:
            stripped = (value or "").strip()
            if not stripped:
                continue
            key = (seed_type, stripped.lower())
            if key in seen_seeds:
                continue
            seen_seeds.add(key)
            cleaned.append((seed_type, stripped))
        if not cleaned:
            raise ValidationError("At least one non-empty seed is required")
        cleaned = cleaned[:MAX_SEEDS]
        count = max(MIN_TRACK_COUNT, min(count or DEFAULT_TRACK_COUNT, MAX_TRACK_COUNT))

        results = await asyncio.gather(
            *(self._seed_pool(user.id, seed_type, value, count) for seed_type, value in cleaned),
            return_exceptions=True,
        )
        pools: list[list[RadioPlanTrack]] = []
        labels: list[str] = []
        notes: list[str] = []
        for (seed_type, value), result in zip(cleaned, results):
            if isinstance(result, BaseException):
                logger.debug("Smart Mix pool failed for %s '%s': %s", seed_type, value, result)
                labels.append(value)
                notes.append(f"{seed_type} '{value}' failed to expand")
                continue
            pool, label, note = result
            labels.append(label)
            if note:
                notes.append(note)
            if pool:
                pools.append(pool)

        if not pools:
            hints = f" ({'; '.join(notes)})" if notes else ""
            raise HTTPException(
                status_code=422,
                detail=(
                    f"No library tracks matched any seed{hints}. "
                    "Smart Mix builds from your own library - try seeds you own music for."
                ),
            )

        tracks = self._blend(pools, count)
        playlist_name = (name or "").strip() or self._default_name(labels)
        playlist = await self._playlist_service.create_playlist(
            playlist_name[:MAX_NAME_LENGTH], user_id=user.id,
        )
        track_dicts = [self._plan_track_to_dict(t) for t in tracks]
        created = await self._playlist_service.add_tracks(playlist.id, user, track_dicts)
        return playlist, created

    @staticmethod
    def _default_name(labels: list[str]) -> str:
        shown = labels[:_NAME_SEED_LIMIT]
        rest = len(labels) - len(shown)
        blend = " + ".join(shown)
        if rest > 0:
            blend += f" + {rest} more"
        return f"{blend} — Smart Mix"

    async def _seed_pool(
        self, user_id: str, seed_type: str, value: str, count: int
    ) -> tuple[list[RadioPlanTrack], str, str | None]:
        """One candidate pool per seed: (tracks, display label, empty-pool note)."""
        if seed_type == "mood":
            return await self._mood_pool(user_id, value, count)

        plan = await self._radio_plan.build_plan(
            user_id,
            RadioPlanRequest(seed_type=seed_type, seed_id=value, mode="library", count=count),
            max_count=MAX_TRACK_COUNT,
        )
        label = plan.title.removeprefix("Radio:").strip() or value
        if seed_type == "genre":
            label = value.title()
        note = None if plan.tracks else f"no library tracks for {seed_type} '{value}'"
        return list(plan.tracks), label, note

    async def _mood_pool(
        self, user_id: str, mood: str, count: int
    ) -> tuple[list[RadioPlanTrack], str, str | None]:
        mood_key = mood.lower()
        label = mood_key.title()
        tags = MOOD_TAG_FAMILIES.get(mood_key)
        if tags is None:
            known = ", ".join(sorted(MOOD_TAG_FAMILIES))
            return [], label, f"unknown mood '{mood}' (available moods: {known})"

        matched = await self._library_tags(tags)
        if not matched:
            return [], label, (
                f"nothing in your library matches the '{mood_key}' mood "
                f"(looked for: {', '.join(tags)})"
            )

        # one library-mode genre station per matched tag, then interleave so the
        # pool spans the whole mood family instead of exhausting one tag first
        per_tag = max(MIN_TRACK_COUNT, -(-count // len(matched)))
        plans = await asyncio.gather(
            *(
                self._radio_plan.build_plan(
                    user_id,
                    RadioPlanRequest(seed_type="genre", seed_id=tag, mode="library", count=per_tag),
                    max_count=MAX_TRACK_COUNT,
                )
                for tag in matched
            ),
            return_exceptions=True,
        )
        tag_pools: list[list[RadioPlanTrack]] = []
        for tag, plan in zip(matched, plans):
            if isinstance(plan, BaseException):
                logger.debug("Smart Mix mood pool failed for tag %s: %s", tag, plan)
                continue
            if plan.tracks:
                tag_pools.append(list(plan.tracks))

        tracks: list[RadioPlanTrack] = []
        seen: set[str] = set()
        indices = [0] * len(tag_pools)
        while len(tracks) < count and any(i < len(p) for i, p in zip(indices, tag_pools)):
            for pool_i, pool in enumerate(tag_pools):
                if len(tracks) >= count:
                    break
                idx = indices[pool_i]
                if idx >= len(pool):
                    continue
                indices[pool_i] = idx + 1
                track = pool[idx]
                key = _track_key(track)
                if key in seen:
                    continue
                seen.add(key)
                tracks.append(track)

        note = None if tracks else f"no library tracks for mood '{mood_key}'"
        return tracks, label, note

    @staticmethod
    def _blend(pools: list[list[RadioPlanTrack]], count: int) -> list[RadioPlanTrack]:
        """Round-robin across seed pools so each seed gets ~equal share.

        Strict pass keeps the engine's diversity caps (max 4 per artist, never
        3 in a row); a track qualifying under several seeds is credited to
        whichever pool reaches it first in rotation (later pools skip it
        without losing their turn), which keeps shares level. A relaxed fill
        then lifts the per-artist cap (small libraries must still fill big
        requests) but KEEPS the consecutive-artist rule.
        """
        selected: list[RadioPlanTrack] = []
        seen: set[str] = set()
        per_artist: dict[str, int] = {}
        indices = [0] * len(pools)

        def _blocked_consecutive(a_key: str) -> bool:
            recent = [_artist_key(t) for t in selected[-_MAX_CONSECUTIVE_SAME_ARTIST:]]
            return (
                len(recent) == _MAX_CONSECUTIVE_SAME_ARTIST
                and all(r == a_key for r in recent)
            )

        progress = True
        while len(selected) < count and progress:
            progress = False
            for pool_i, pool in enumerate(pools):
                if len(selected) >= count:
                    break
                while indices[pool_i] < len(pool):
                    track = pool[indices[pool_i]]
                    indices[pool_i] += 1
                    key = _track_key(track)
                    if key in seen:
                        continue
                    a_key = _artist_key(track)
                    if per_artist.get(a_key, 0) >= _MAX_PER_ARTIST:
                        continue
                    if _blocked_consecutive(a_key):
                        continue
                    seen.add(key)
                    per_artist[a_key] = per_artist.get(a_key, 0) + 1
                    selected.append(track)
                    progress = True
                    break

        if len(selected) < count:
            remaining = [t for pool in pools for t in pool if _track_key(t) not in seen]
            progress = True
            while len(selected) < count and remaining and progress:
                progress = False
                for i, track in enumerate(remaining):
                    key = _track_key(track)
                    if key in seen:
                        remaining.pop(i)
                        progress = True
                        break
                    if _blocked_consecutive(_artist_key(track)):
                        continue
                    seen.add(key)
                    selected.append(track)
                    remaining.pop(i)
                    progress = True
                    break
        return selected

    async def _library_tags(self, tags: list[str]) -> list[str]:
        """The subset of ``tags`` the user's library can actually seed from."""
        matched: list[str] = []
        for tag in tags:
            if await self._tag_in_library(tag):
                matched.append(tag)
        return matched

    async def _tag_in_library(self, tag: str) -> bool:
        if self._genre_index is not None:
            try:
                by_genre = await self._genre_index.get_artists_for_genres([tag])
                if by_genre.get(tag.strip().lower()):
                    return True
            except Exception as e:  # noqa: BLE001
                logger.debug("Smart Mix genre-index check failed for %s: %s", tag, e)
        if self._library_db is not None:
            try:
                rows = await self._library_db.get_files_by_genre(tag, limit=1)
                if rows:
                    return True
            except Exception as e:  # noqa: BLE001
                logger.debug("Smart Mix file-tag check failed for %s: %s", tag, e)
        return False

    @staticmethod
    def _plan_track_to_dict(track: RadioPlanTrack) -> dict[str, Any]:
        """Library plan track -> playlist track snapshot (same shape add_tracks expects)."""
        duration = round(track.duration_s) if track.duration_s else None
        cover_url = (
            f"/api/v1/covers/release-group/{track.album_mbid}?size=250"
            if track.album_mbid
            else None
        )
        return {
            "track_name": track.track_name,
            "artist_name": track.artist_name,
            "album_name": track.album_name or "",
            "album_id": track.album_mbid,
            "artist_id": track.artist_mbid or None,
            "track_source_id": track.local_file_id,
            "cover_url": cover_url,
            "source_type": "local" if track.local_file_id else "",
            "available_sources": ["local"] if track.local_file_id else None,
            "format": track.file_format,
            "track_number": None,
            "disc_number": None,
            "duration": duration,
            "library_file_id": track.local_file_id,
        }
