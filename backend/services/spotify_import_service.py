"""Spotify playlist import."""

from __future__ import annotations

import asyncio
import logging
from typing import TYPE_CHECKING

from infrastructure.queue.priority_queue import RequestPriority
from repositories.musicbrainz_album import _pick_best_release_group
from repositories.musicbrainz_base import mb_api_get
from repositories.async_playlist_repository import AsyncPlaylistRepository

if TYPE_CHECKING:
    from repositories.musicbrainz_repository import MusicBrainzRepository
    from repositories.playlist_repository import PlaylistRepository
    from services.per_user_client_factory import PerUserClientFactory
    from services.playlist_service import PlaylistService

logger = logging.getLogger(__name__)

# Maximum concurrent MusicBrainz ISRC lookups at any one time.
# The module-level mb_rate_limiter naturally throttles to 1 req/sec;
# this just caps the fan-out so we don't queue hundreds of coroutines
# at once for very large playlists.
_MB_CONCURRENCY = 4


class SpotifyNotLinkedError(Exception):
    pass


def _best_image_url(images: list[dict], min_size: int = 250) -> str | None:
    if not images:
        return None
    sorted_imgs = sorted(images, key=lambda i: i.get("width") or 0)
    for img in sorted_imgs:
        if (img.get("width") or 0) >= min_size:
            return img.get("url")
    return sorted_imgs[-1].get("url")


class SpotifyImportService:
    def __init__(
        self,
        client_factory: PerUserClientFactory,
        playlist_repo: PlaylistRepository,
        mb_repo: MusicBrainzRepository,
        playlist_service: PlaylistService,
    ) -> None:
        self._client_factory = client_factory
        self._async_repo = AsyncPlaylistRepository(playlist_repo)
        self._mb_repo = mb_repo
        self._playlist_service = playlist_service

    async def _get_client(self, user_id: str):
        client = await self._client_factory.resolve_spotify(user_id)
        if client is None:
            raise SpotifyNotLinkedError("Spotify account not linked")
        return client

    async def list_playlists(self, user_id: str) -> list[dict]:
        client = await self._get_client(user_id)

        spotify_user_id = client.spotify_user_id
        if not spotify_user_id:
            me = await client.get_current_user()
            spotify_user_id = me.get("id", "")

        raw = await client.get_user_playlists()

        user_playlists = await self._async_repo.get_all_playlists(user_id)
        imported_mapping: dict[str, str] = {
            pl.source_ref[len("spotify:"):]: pl.id
            for pl in user_playlists
            if pl.source_ref and pl.source_ref.startswith("spotify:")
        }

        result = []
        for p in raw:
            pid = p.get("id") or ""
            owner = p.get("owner") or {}
            if owner.get("id") != spotify_user_id:
                continue
            images = p.get("images") or []
            cover_url = _best_image_url(images)
            result.append(
                {
                    "id": pid,
                    "name": p.get("name") or "",
                    "description": p.get("description") or "",
                    "track_count": (p.get("tracks") or {}).get("total", 0),
                    "cover_url": cover_url,
                    "owner": owner.get("display_name") or "",
                    "imported_playlist_id": imported_mapping.get(pid),
                }
            )
        return result

    async def ensure_playlist_record(
        self, user_id: str, spotify_playlist_id: str, name: str
    ) -> str:
        source_ref = f"spotify:{spotify_playlist_id}"
        existing = await self._playlist_service.get_by_source_ref(source_ref, user_id)
        if existing:
            return existing.id
        record = await self._playlist_service.create_playlist(
            name or "Spotify Playlist", source_ref=source_ref, user_id=user_id
        )
        return record.id

    async def populate_playlist(
        self, user_id: str, spotify_playlist_id: str, playlist_id: str
    ) -> None:
        client = await self._get_client(user_id)

        _pl_info, raw_tracks = await asyncio.gather(
            client.get_playlist(spotify_playlist_id),
            client.get_playlist_tracks(spotify_playlist_id),
        )

        album_to_mbid = await self._resolve_album_mbids(raw_tracks)

        existing_tracks = await self._async_repo.get_tracks(playlist_id)
        if existing_tracks:
            await self._async_repo.remove_tracks(
                playlist_id, [t.id for t in existing_tracks]
            )

        track_dicts = []
        for track in raw_tracks:
            album = track.get("album") or {}
            album_spotify_id = album.get("id") or ""
            mbid = album_to_mbid.get(album_spotify_id)
            artist_name = ", ".join(
                a.get("name", "") for a in (track.get("artists") or []) if a.get("name")
            )
            if mbid:
                cover_url = f"/api/v1/covers/release-group/{mbid}?size=250"
            else:
                cover_url = _best_image_url(album.get("images") or [])
            duration_ms = track.get("duration_ms")
            track_dicts.append(
                {
                    "track_name": track.get("name") or "",
                    "artist_name": artist_name,
                    "album_name": album.get("name") or "",
                    "album_id": mbid or "",
                    "source_type": "",
                    "track_number": track.get("track_number"),
                    "disc_number": track.get("disc_number"),
                    "duration": duration_ms // 1000 if duration_ms else None,
                    "cover_url": cover_url,
                }
            )

        await self._async_repo.add_tracks(playlist_id, track_dicts)
        logger.info(
            f"Imported Spotify playlist {spotify_playlist_id} - internal {playlist_id} ({len(track_dicts)} tracks)"
        )

    async def import_playlist(self, user_id: str, spotify_playlist_id: str) -> str:
        client = await self._get_client(user_id)
        pl_info = await client.get_playlist(spotify_playlist_id)
        name = (pl_info.get("name") or "").strip() or "Spotify Playlist"
        playlist_id = await self.ensure_playlist_record(user_id, spotify_playlist_id, name)
        await self.populate_playlist(user_id, spotify_playlist_id, playlist_id)
        return playlist_id

    async def _resolve_album_mbids(
        self, raw_tracks: list[dict]
    ) -> dict[str, str | None]:
        album_isrc: dict[str, str | None] = {}
        album_info: dict[str, tuple[str, str]] = {}
        for track in raw_tracks:
            album = track.get("album") or {}
            album_id = album.get("id") or ""
            if not album_id or album_id in album_isrc:
                continue
            album_isrc[album_id] = (track.get("external_ids") or {}).get("isrc")
            artist = ", ".join(
                a.get("name", "") for a in (track.get("artists") or []) if a.get("name")
            )
            album_info[album_id] = (artist, album.get("name") or "")

        semaphore = asyncio.Semaphore(_MB_CONCURRENCY)

        async def resolve_one(album_id: str) -> tuple[str, str | None]:
            async with semaphore:
                isrc = album_isrc[album_id]
                artist, album_name = album_info[album_id]
                mbid = await self._resolve_mbid(isrc, artist, album_name)
                return album_id, mbid

        results = await asyncio.gather(*[resolve_one(aid) for aid in album_isrc])
        return dict(results)

    async def _resolve_mbid(
        self, isrc: str | None, artist: str, album_name: str
    ) -> str | None:
        if isrc:
            try:
                data = await mb_api_get(
                    f"/isrc/{isrc}",
                    priority=RequestPriority.BACKGROUND_SYNC,
                )
                recordings: list[dict] = data.get("recordings") or []
                if isinstance(recordings, dict):
                    recordings = [recordings]
                for rec in recordings:
                    rec_id = rec.get("id")
                    if not rec_id:
                        continue
                    mbid = await self._mb_repo.resolve_recording_to_release_group(rec_id)
                    if mbid:
                        return mbid
                all_releases: list[dict] = []
                for rec in recordings:
                    all_releases.extend(rec.get("releases") or [])
                best = _pick_best_release_group(all_releases)
                if best:
                    return best[0]
            except Exception:  # noqa: BLE001
                pass

        if album_name:
            try:
                query = f"{artist} {album_name}".strip()
                results = await self._mb_repo.search_albums(
                    query,
                    limit=3,
                    include_all_types=False,
                )
                if results:
                    return results[0].musicbrainz_id
            except Exception:  # noqa: BLE001
                pass

        return None
