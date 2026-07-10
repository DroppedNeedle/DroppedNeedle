"""Smart Mix: turn a seed (artist / genre / mood) into a real, saved playlist.

Thin orchestration over the radio-plan engine: ``RadioPlanService``'s
library-first pool does the actual track selection (genre-index artists,
file-tag genre matches, shared-genre adjacency, diversity caps). This service
maps mood seeds onto curated genre/tag families, runs the plan(s) in library
mode, and persists the result as a native playlist owned by the caller.
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

# Curated mood -> genre/tag families, matched against the user's OWN library
# (genre index + file tags). Only tags the library actually contains are used
# as station seeds; a mood with zero matches is a 422, not an empty playlist.
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


class SmartPlaylistService:
    """Generate-and-save playlists from a single seed via the radio-plan engine."""

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
        seed_type: str,
        seed: str,
        count: int = DEFAULT_TRACK_COUNT,
        name: str | None = None,
    ) -> tuple[PlaylistRecord, list[PlaylistTrackRecord]]:
        """Build a library Smart Mix and persist it as a playlist owned by ``user``."""
        seed = (seed or "").strip()
        if not seed:
            raise ValidationError("seed must be non-empty")
        count = max(MIN_TRACK_COUNT, min(count or DEFAULT_TRACK_COUNT, MAX_TRACK_COUNT))

        if seed_type == "mood":
            tracks, default_name = await self._mood_tracks(user.id, seed, count)
        else:  # artist | genre (schema-validated)
            plan = await self._radio_plan.build_plan(
                user.id,
                RadioPlanRequest(seed_type=seed_type, seed_id=seed, mode="library", count=count),
                max_count=MAX_TRACK_COUNT,
            )
            tracks = plan.tracks
            base = plan.title.removeprefix("Radio:").strip() or seed
            default_name = f"{base} — Smart Mix"

        if not tracks:
            raise HTTPException(
                status_code=422,
                detail=(
                    f"No library tracks matched the {seed_type} seed '{seed}'. "
                    "Smart Mix builds from your own library - try a seed you own music for."
                ),
            )

        playlist_name = (name or "").strip() or default_name
        playlist = await self._playlist_service.create_playlist(
            playlist_name[:MAX_NAME_LENGTH], user_id=user.id,
        )
        track_dicts = [self._plan_track_to_dict(t) for t in tracks[:count]]
        created = await self._playlist_service.add_tracks(playlist.id, user, track_dicts)
        return playlist, created

    async def _mood_tracks(
        self, user_id: str, mood: str, count: int
    ) -> tuple[list[RadioPlanTrack], str]:
        mood_key = mood.lower()
        tags = MOOD_TAG_FAMILIES.get(mood_key)
        if tags is None:
            known = ", ".join(sorted(MOOD_TAG_FAMILIES))
            raise HTTPException(
                status_code=422, detail=f"Unknown mood '{mood}'. Available moods: {known}",
            )

        matched = await self._library_tags(tags)
        if not matched:
            raise HTTPException(
                status_code=422,
                detail=(
                    f"Nothing in your library matches the '{mood_key}' mood yet "
                    f"(looked for: {', '.join(tags)}). Add some matching music and try again."
                ),
            )

        # one library-mode genre station per matched tag, then interleave so the
        # mix spans the whole mood family instead of exhausting one tag first
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
        pools: list[list[RadioPlanTrack]] = []
        for tag, plan in zip(matched, plans):
            if isinstance(plan, BaseException):
                logger.debug("Smart Mix mood pool failed for tag %s: %s", tag, plan)
                continue
            if plan.tracks:
                pools.append(list(plan.tracks))

        tracks: list[RadioPlanTrack] = []
        seen: set[str] = set()
        indices = [0] * len(pools)
        while len(tracks) < count and any(i < len(p) for i, p in zip(indices, pools)):
            for pool_i, pool in enumerate(pools):
                if len(tracks) >= count:
                    break
                idx = indices[pool_i]
                if idx >= len(pool):
                    continue
                indices[pool_i] = idx + 1
                track = pool[idx]
                key = f"{track.artist_name.lower()}|{track.track_name.lower()}"
                if key in seen:
                    continue
                seen.add(key)
                tracks.append(track)

        return tracks, f"{mood_key.title()} — Smart Mix"

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
