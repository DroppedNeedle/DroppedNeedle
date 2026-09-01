"""Spotify playlist import."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable
from typing import TYPE_CHECKING, Any

import httpx

from infrastructure.degradation import try_get_degradation_context
from infrastructure.http.client import get_spotify_cover_http_client
from infrastructure.integration_result import IntegrationResult
from infrastructure.queue.priority_queue import RequestPriority
from infrastructure.validators import validate_spotify_cover_url
from repositories.musicbrainz_album import _pick_best_release_group
from repositories.musicbrainz_base import (
    capture_mb_source_context,
    clear_mb_response_context,
    get_mb_response_context,
    is_mb_source_current,
    mb_api_get,
)
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


_SOURCE = "spotify"

# Network read bound for one playlist cover. Deliberately looser than the
# 2 MB storage cap PlaylistService enforces: this only stops us reading an
# unbounded response off the wire; storage validation stays authoritative.
MAX_COVER_FETCH_BYTES = 5 * 1024 * 1024

CoverFetcher = Callable[[str], Awaitable[tuple[bytes, str] | None]]


def _record_degradation(msg: str) -> None:
    ctx = try_get_degradation_context()
    if ctx is not None:
        ctx.record(IntegrationResult.error(source=_SOURCE, msg=msg))


async def fetch_spotify_playlist_cover(
    url: str, http_client: httpx.AsyncClient
) -> tuple[bytes, str] | None:
    """Fetch and validate a Spotify CDN playlist image.

    Single attempt, no redirects (a scdn.co URL must answer directly), HTTPS
    host allowlist, ``image/*`` content type, and a bounded streamed read that
    aborts past :data:`MAX_COVER_FETCH_BYTES` without buffering the excess.
    Returns ``(image_bytes, content_type)``, or ``None`` when the response is
    unusable. Network errors propagate; the caller degrades.
    """
    if not validate_spotify_cover_url(url):
        return None
    async with http_client.stream("GET", url, follow_redirects=False) as response:
        if response.is_redirect or response.status_code != 200:
            return None
        content_type = (
            response.headers.get("content-type", "").split(";")[0].strip().lower()
        )
        if not content_type.startswith("image/"):
            return None
        declared = response.headers.get("content-length")
        if (
            declared
            and declared.lstrip().isdigit()
            and int(declared) > MAX_COVER_FETCH_BYTES
        ):
            return None
        buffer = bytearray()
        async for chunk in response.aiter_bytes():
            buffer.extend(chunk)
            if len(buffer) > MAX_COVER_FETCH_BYTES:
                return None
    return bytes(buffer), content_type


def cover_fetcher_for(http_client: httpx.AsyncClient) -> CoverFetcher:
    """Bind the bounded cover fetch to a client - the factory-built named
    client in production, a MockTransport-served client in tests."""

    async def _fetch(url: str) -> tuple[bytes, str] | None:
        return await fetch_spotify_playlist_cover(url, http_client)

    return _fetch


class SpotifyImportService:
    def __init__(
        self,
        client_factory: PerUserClientFactory,
        playlist_repo: PlaylistRepository | None,
        mb_repo: MusicBrainzRepository,
        playlist_service: PlaylistService,
        async_playlist_repo: Any | None = None,
        cover_fetcher: CoverFetcher | None = None,
    ) -> None:
        self._client_factory = client_factory
        if async_playlist_repo is None and playlist_repo is None:
            raise ValueError("A playlist repository is required.")
        self._async_repo = (
            async_playlist_repo
            if async_playlist_repo is not None
            else AsyncPlaylistRepository(playlist_repo)
        )
        self._mb_repo = mb_repo
        self._playlist_service = playlist_service
        self._cover_fetcher: CoverFetcher | None = cover_fetcher

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
            pl.source_ref[len("spotify:") :]: pl.id
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
        await self._persist_playlist_cover(
            user_id, spotify_playlist_id, playlist_id, _pl_info
        )
        logger.info(
            f"Imported Spotify playlist {spotify_playlist_id} - internal {playlist_id} ({len(track_dicts)} tracks)"
        )

    def _cover_fetch(self) -> CoverFetcher:
        """Lazily bind the default named factory client on first use, so pure
        unit tests that never touch covers don't build an HTTP client."""
        if self._cover_fetcher is None:
            self._cover_fetcher = cover_fetcher_for(get_spotify_cover_http_client())
        return self._cover_fetcher

    async def _persist_playlist_cover(
        self,
        user_id: str,
        spotify_playlist_id: str,
        playlist_id: str,
        pl_info: dict,
    ) -> None:
        """Optional enrichment: store the picked provider image as the local
        playlist cover. Any failure degrades (recorded into the request-scoped
        DegradationContext when one is active) and leaves the import untouched -
        artwork must never fail a playlist import; no cover is normal."""
        cover_url = _best_image_url(pl_info.get("images") or [])
        if not cover_url:
            return
        try:
            fetched = await self._cover_fetch()(cover_url)
            if fetched is None:
                msg = (
                    f"Spotify playlist cover rejected for {spotify_playlist_id} "
                    f"({cover_url})"
                )
                _record_degradation(msg)
                logger.warning(
                    "spotify.playlist_cover action=rejected spotify_playlist=%s",
                    spotify_playlist_id,
                )
                return
            data, content_type = fetched
            stored = await self._playlist_service.set_imported_cover(
                playlist_id, user_id, data, content_type
            )
            logger.info(
                "spotify.playlist_cover action=%s playlist=%s",
                "stored" if stored else "kept_existing",
                playlist_id,
            )
        except Exception as exc:  # noqa: BLE001 - optional artwork never fails the import
            _record_degradation(
                f"Spotify playlist cover fetch failed for {spotify_playlist_id}: {exc}"
            )
            logger.warning(
                "spotify.playlist_cover action=failed spotify_playlist=%s error=%s",
                spotify_playlist_id,
                exc,
            )

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
        clear_mb_response_context()
        operation_context = capture_mb_source_context()
        if isrc:
            canonical_store = getattr(self._mb_repo, "mb_canonical_store", None)
            if canonical_store is not None:
                try:
                    existing = await canonical_store.get_recordings_by_isrc(
                        isrc,
                        source_context=operation_context,
                    )
                    if not is_mb_source_current(operation_context):
                        existing = []
                except Exception:  # noqa: BLE001 - durable miss falls through to wire
                    existing = []
                if not isinstance(existing, (list, tuple, set)):
                    existing = []

                # Only inspect the repository memory tier before paying for /isrc.
                # A durable ISRC row is an index, not permission to issue another
                # recording wire for every candidate.
                cache_lookup = getattr(
                    self._mb_repo, "get_cached_recording_to_release_group", None
                )
                if callable(cache_lookup):
                    for rec_id in sorted(
                        {str(value).casefold() for value in existing if value}
                    ):
                        try:
                            mbid = await cache_lookup(rec_id)
                        except Exception:  # noqa: BLE001 - try the next known row
                            continue
                        if mbid:
                            return mbid

            operation_context = capture_mb_source_context()
            try:
                data = await mb_api_get(
                    f"/isrc/{isrc}",
                    priority=RequestPriority.BACKGROUND_SYNC,
                    source_context=operation_context,
                )
                response_context = get_mb_response_context() or operation_context
                if response_context != operation_context or not is_mb_source_current(
                    operation_context
                ):
                    raise RuntimeError("MusicBrainz source changed during ISRC lookup")
                recordings: list[dict] = data.get("recordings") or []
                if isinstance(recordings, dict):
                    recordings = [recordings]
                # ST2 P1: bank ISRC -> recording ids durably (write-through)
                # only for the source generation that answered.
                if canonical_store is not None and operation_context is not None:
                    try:
                        await canonical_store.save_isrc_recordings(
                            [(isrc, rec["id"]) for rec in recordings if rec.get("id")],
                            source_context=operation_context,
                        )
                    except Exception:  # noqa: BLE001
                        pass  # write-through must never break the import
                for rec in recordings:
                    if not is_mb_source_current(operation_context):
                        raise RuntimeError(
                            "MusicBrainz source changed during recording resolution"
                        )
                    rec_id = rec.get("id")
                    if not rec_id:
                        continue
                    mbid = await self._mb_repo.resolve_recording_to_release_group(
                        rec_id
                    )
                    if mbid:
                        return mbid
                if not is_mb_source_current(operation_context):
                    raise RuntimeError(
                        "MusicBrainz source changed during ISRC release selection"
                    )
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
                results = await self._mb_repo.search_release_groups(
                    artist,
                    album_name,
                    limit=3,
                    include_all_types=False,
                )
                if results:
                    return results[0].musicbrainz_id
            except Exception:  # noqa: BLE001
                pass

        return None
