from __future__ import annotations

import asyncio
import logging
import re
import time
import unicodedata
from typing import TYPE_CHECKING

from api.v1.schemas.plex import (
    PlexAlbumDetail,
    PlexAlbumMatch,
    PlexAlbumSummary,
    PlexAnalyticsItem,
    PlexAnalyticsResponse,
    PlexArtistIndexEntry,
    PlexArtistIndexResponse,
    PlexArtistSummary,
    PlexDiscoveryAlbum,
    PlexDiscoveryHub,
    PlexDiscoveryResponse,
    PlexHistoryEntrySchema,
    PlexHistoryResponse,
    PlexHubResponse,
    PlexImportResult,
    PlexLibraryStats,
    PlexPlaylistDetail,
    PlexPlaylistSummary,
    PlexPlaylistTrack,
    PlexSearchResponse,
    PlexSessionInfo,
    PlexSessionsResponse,
    PlexTrackInfo,
)
from infrastructure.cover_urls import prefer_artist_cover_url, prefer_release_group_cover_url
from core.exceptions import ExternalServiceError
from repositories.plex_models import PlexAlbum, PlexArtist, PlexTrack, extract_mbid_from_guids
from repositories.protocols.plex import PlexRepositoryProtocol
from services.preferences_service import PreferencesService

if TYPE_CHECKING:
    from infrastructure.persistence import LibraryDB, MBIDStore

logger = logging.getLogger(__name__)

_CONCURRENCY_LIMIT = 5
_NEGATIVE_CACHE_TTL = 14400


def _clean_album_name(name: str) -> str:
    cleaned = name.strip()
    cleaned = re.sub(
        r'\s*[\(\[][^)\]]*(?:remaster|deluxe|edition|bonus|expanded|mono|stereo|anniversary)[^)\]]*[\)\]]',
        '', cleaned, flags=re.IGNORECASE,
    )
    cleaned = re.sub(r'^\d{4}\s*[-–—]\s*', '', cleaned)
    cleaned = re.sub(r'\s*-\s*EP$', '', cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r'\s*\[[^\]]*\]\s*$', '', cleaned)
    return cleaned.strip()


def _normalize(text: str) -> str:
    text = unicodedata.normalize("NFKD", text)
    text = text.encode("ascii", "ignore").decode("ascii")
    text = re.sub(r"[^a-z0-9]", "", text.lower())
    return text


def _parse_decade(decade: str | None) -> tuple[int | None, int]:
    if not decade:
        return None, 0
    stripped = decade.rstrip("s")
    try:
        start = int(stripped)
    except ValueError:
        return None, 0
    return start, start + 9


def _sort_albums(albums: list[PlexAlbum], sort: str) -> list[PlexAlbum]:
    parts = sort.split(":", 1)
    field = parts[0] if parts else "titleSort"
    direction = parts[1] if len(parts) > 1 else "asc"
    reverse = direction == "desc"

    if field in ("titleSort", "title"):
        return sorted(albums, key=lambda a: a.title.lower(), reverse=reverse)
    if field == "addedAt":
        return sorted(albums, key=lambda a: a.addedAt, reverse=reverse)
    if field == "year":
        return sorted(albums, key=lambda a: a.year, reverse=reverse)
    if field == "viewCount":
        return sorted(albums, key=lambda a: a.viewCount, reverse=reverse)
    if field == "userRating":
        return sorted(albums, key=lambda a: a.userRating, reverse=reverse)
    if field == "lastViewedAt":
        return sorted(albums, key=lambda a: a.lastViewedAt, reverse=reverse)
    return sorted(albums, key=lambda a: a.title.lower(), reverse=reverse)


class PlexLibraryService:

    def __init__(
        self,
        plex_repo: PlexRepositoryProtocol,
        preferences_service: PreferencesService,
        library_db: 'LibraryDB | None' = None,
        mbid_store: 'MBIDStore | None' = None,
    ):
        self._plex = plex_repo
        self._preferences = preferences_service
        self._library_db = library_db
        self._mbid_store = mbid_store
        self._album_mbid_cache: dict[str, str | tuple[None, float]] = {}
        self._artist_mbid_cache: dict[str, str | tuple[None, float]] = {}
        self._mbid_to_plex_id: dict[str, str] = {}
        self._lidarr_album_index: dict[str, tuple[str, str]] = {}
        self._analytics_cache: PlexAnalyticsResponse | None = None
        self._analytics_cache_ts: float = 0.0
        self._lidarr_artist_index: dict[str, str] = {}
        self._dirty = False
        self._stats_cache: PlexLibraryStats | None = None
        self._stats_cache_ts: float = 0.0

    def lookup_plex_id(self, mbid: str) -> str | None:
        return self._mbid_to_plex_id.get(mbid)

    def _get_configured_section_ids(self) -> list[str]:
        try:
            conn = self._preferences.get_plex_connection_raw()
            if conn and conn.enabled and conn.music_library_ids:
                return list(conn.music_library_ids)
        except Exception:  # noqa: BLE001
            pass
        return []

    async def _resolve_album_mbid(self, name: str, artist: str) -> str | None:
        if not name or not artist:
            return None
        cache_key = f"{_normalize(name)}:{_normalize(artist)}"
        if cache_key in self._album_mbid_cache:
            cached = self._album_mbid_cache[cache_key]
            if isinstance(cached, str):
                return cached
            if isinstance(cached, tuple):
                _, ts = cached
                if time.time() - ts < _NEGATIVE_CACHE_TTL:
                    return None
                del self._album_mbid_cache[cache_key]
            elif cached is None:
                del self._album_mbid_cache[cache_key]

        match = self._lidarr_album_index.get(cache_key)
        if match:
            self._album_mbid_cache[cache_key] = match[0]
            self._dirty = True
            return match[0]

        clean_key = f"{_normalize(_clean_album_name(name))}:{_normalize(artist)}"
        if clean_key != cache_key:
            match = self._lidarr_album_index.get(clean_key)
            if match:
                self._album_mbid_cache[cache_key] = match[0]
                self._dirty = True
                return match[0]

        self._album_mbid_cache[cache_key] = (None, time.time())
        self._dirty = True
        return None

    async def _resolve_artist_mbid(self, name: str) -> str | None:
        if not name:
            return None
        cache_key = _normalize(name)
        if cache_key in self._artist_mbid_cache:
            cached = self._artist_mbid_cache[cache_key]
            if isinstance(cached, str):
                return cached
            if isinstance(cached, tuple):
                _, ts = cached
                if time.time() - ts < _NEGATIVE_CACHE_TTL:
                    return None
                del self._artist_mbid_cache[cache_key]
            elif cached is None:
                del self._artist_mbid_cache[cache_key]

        match = self._lidarr_artist_index.get(cache_key)
        if match:
            self._artist_mbid_cache[cache_key] = match
            self._dirty = True
            return match

        self._artist_mbid_cache[cache_key] = (None, time.time())
        self._dirty = True
        return None

    def _track_to_info(self, track: PlexTrack) -> PlexTrackInfo:
        codec: str | None = None
        bitrate: int | None = None
        audio_channels: int | None = None
        container: str | None = None
        part_key: str | None = None
        if track.Media:
            media = track.Media[0]
            codec = media.audioCodec or None
            bitrate = media.bitrate or None
            audio_channels = media.audioChannels or None
            container = media.container or None
            if media.Part:
                part_key = media.Part[0].key
        return PlexTrackInfo(
            plex_id=track.ratingKey,
            title=track.title,
            track_number=track.index,
            disc_number=track.parentIndex or 1,
            duration_seconds=track.duration / 1000.0 if track.duration else 0.0,
            album_name=track.parentTitle,
            artist_name=track.grandparentTitle,
            codec=codec,
            bitrate=bitrate,
            audio_channels=audio_channels,
            container=container,
            part_key=part_key,
            image_url=f"/api/v1/plex/thumb/{track.parentRatingKey}" if track.parentRatingKey else None,
        )

    async def _album_to_summary(self, album: PlexAlbum) -> PlexAlbumSummary:
        plex_mbid = extract_mbid_from_guids(album.Guid)
        mbid = plex_mbid or await self._resolve_album_mbid(album.title, album.parentTitle)
        if mbid:
            self._mbid_to_plex_id[mbid] = album.ratingKey
        artist_mbid = await self._resolve_artist_mbid(album.parentTitle) if album.parentTitle else None

        fallback = f"/api/v1/plex/thumb/{album.ratingKey}" if album.thumb else None
        image_url = prefer_release_group_cover_url(mbid, fallback, size=500)

        return PlexAlbumSummary(
            plex_id=album.ratingKey,
            name=album.title,
            artist_name=album.parentTitle,
            year=album.year or None,
            track_count=album.leafCount,
            image_url=image_url,
            musicbrainz_id=mbid or None,
            artist_musicbrainz_id=artist_mbid,
            last_viewed_at=album.lastViewedAt or 0,
        )

    async def _build_artist_summary(self, artist: PlexArtist) -> PlexArtistSummary:
        plex_mbid = extract_mbid_from_guids(artist.Guid)
        mbid = plex_mbid or await self._resolve_artist_mbid(artist.title)
        image_url = prefer_artist_cover_url(mbid, None, size=500)
        return PlexArtistSummary(
            plex_id=artist.ratingKey,
            name=artist.title,
            image_url=image_url,
            musicbrainz_id=mbid or None,
        )

    async def get_albums(
        self,
        size: int = 50,
        offset: int = 0,
        sort: str = "titleSort:asc",
        genre: str | None = None,
        mood: str | None = None,
        decade: str | None = None,
    ) -> tuple[list[PlexAlbumSummary], int]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return [], 0

        decade_start, decade_end = _parse_decade(decade)

        if len(section_ids) == 1:
            albums, total = await self._plex.get_albums(
                section_id=section_ids[0], size=size, offset=offset, sort=sort, genre=genre, mood=mood, decade=decade,
            )
            filtered = [a for a in albums if a.title and a.title != "Unknown"]
            summaries = await asyncio.gather(*(self._album_to_summary(a) for a in filtered))
            return list(summaries), total

        fetch_limit = offset + size
        all_albums: list[PlexAlbum] = []
        total = 0
        seen: set[str] = set()
        for sid in section_ids:
            albums, section_total = await self._plex.get_albums(
                section_id=sid, size=fetch_limit, offset=0, sort=sort, genre=genre, mood=mood, decade=decade,
            )
            for a in albums:
                if a.ratingKey not in seen:
                    seen.add(a.ratingKey)
                    all_albums.append(a)
            total += section_total

        all_albums = _sort_albums(all_albums, sort)

        if decade_start is not None:
            all_albums = [a for a in all_albums if a.year and decade_start <= a.year <= decade_end]
            total = len(all_albums)

        page = all_albums[offset : offset + size]
        filtered = [a for a in page if a.title and a.title != "Unknown"]
        summaries = await asyncio.gather(*(self._album_to_summary(a) for a in filtered))
        return list(summaries), total

    async def get_album_detail(self, rating_key: str) -> PlexAlbumDetail | None:
        try:
            album = await self._plex.get_album_metadata(rating_key)
            tracks_raw = await self._plex.get_album_tracks(rating_key)
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch Plex album %s", rating_key, exc_info=True)
            return None

        tracks = [self._track_to_info(t) for t in tracks_raw]

        plex_mbid = extract_mbid_from_guids(album.Guid)
        mbid = plex_mbid or await self._resolve_album_mbid(album.title, album.parentTitle)
        artist_mbid = await self._resolve_artist_mbid(album.parentTitle) if album.parentTitle else None

        fallback = f"/api/v1/plex/thumb/{album.ratingKey}" if album.thumb else None
        image_url = prefer_release_group_cover_url(mbid, fallback, size=500)

        genres = [g.tag for g in album.Genre if g.tag]

        return PlexAlbumDetail(
            plex_id=album.ratingKey,
            name=album.title,
            artist_name=album.parentTitle,
            year=album.year or None,
            track_count=len(tracks),
            image_url=image_url,
            musicbrainz_id=mbid or None,
            artist_musicbrainz_id=artist_mbid,
            tracks=tracks,
            genres=genres,
        )

    async def get_artists(self) -> list[PlexArtistSummary]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return []

        all_artists: list[PlexArtist] = []
        for sid in section_ids:
            offset = 0
            while True:
                batch = await self._plex.get_artists(section_id=sid, size=500, offset=offset)
                if not batch:
                    break
                all_artists.extend(batch)
                if len(batch) < 500:
                    break
                offset += 500

        summaries = await asyncio.gather(*(self._build_artist_summary(a) for a in all_artists))
        return list(summaries)

    async def browse_artists(
        self,
        size: int = 48,
        offset: int = 0,
        sort: str = "titleSort:asc",
        search: str = "",
    ) -> tuple[list[PlexArtistSummary], int]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return [], 0

        sid = section_ids[0]
        artists = await self._plex.get_artists(section_id=sid, size=size, offset=offset, search=search)
        total = await self._plex.get_artist_count(sid)
        summaries = await asyncio.gather(*(self._build_artist_summary(a) for a in artists))
        return list(summaries), total

    async def get_artists_index(self) -> PlexArtistIndexResponse:
        all_summaries = await self.get_artists()

        groups: dict[str, list[PlexArtistSummary]] = {}
        for artist in all_summaries:
            letter = artist.name[0].upper() if artist.name else "#"
            if not letter.isalpha():
                letter = "#"
            groups.setdefault(letter, []).append(artist)

        entries = [
            PlexArtistIndexEntry(name=letter, artists=artists)
            for letter, artists in sorted(groups.items())
        ]
        return PlexArtistIndexResponse(index=entries)

    async def browse_tracks(
        self,
        size: int = 48,
        offset: int = 0,
        sort: str = "titleSort:asc",
        search: str = "",
    ) -> tuple[list[PlexTrackInfo], int]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return [], 0

        sid = section_ids[0]
        tracks, total = await self._plex.get_tracks(
            section_id=sid, size=size, offset=offset, sort=sort, search=search
        )
        return [self._track_to_info(t) for t in tracks], total

    async def search(self, query: str) -> PlexSearchResponse:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return PlexSearchResponse()

        all_albums_raw: list[PlexAlbum] = []
        all_artists_raw: list[PlexArtist] = []
        all_tracks_raw: list[PlexTrack] = []
        seen_albums: set[str] = set()
        seen_artists: set[str] = set()
        seen_tracks: set[str] = set()

        for sid in section_ids:
            result = await self._plex.search(query, section_id=sid)
            for a in result.get("albums", []):
                if a.ratingKey not in seen_albums:
                    seen_albums.add(a.ratingKey)
                    all_albums_raw.append(a)
            for a in result.get("artists", []):
                if a.ratingKey not in seen_artists:
                    seen_artists.add(a.ratingKey)
                    all_artists_raw.append(a)
            for t in result.get("tracks", []):
                if t.ratingKey not in seen_tracks:
                    seen_tracks.add(t.ratingKey)
                    all_tracks_raw.append(t)

        filtered_albums = [a for a in all_albums_raw if a.title and a.title != "Unknown"]
        albums_task = asyncio.gather(*(self._album_to_summary(a) for a in filtered_albums))
        artists_task = asyncio.gather(*(self._build_artist_summary(a) for a in all_artists_raw))
        albums, artists = await asyncio.gather(albums_task, artists_task)
        tracks = [self._track_to_info(t) for t in all_tracks_raw]

        return PlexSearchResponse(
            albums=list(albums),
            artists=list(artists),
            tracks=tracks,
        )

    async def get_recent(self, limit: int = 20) -> list[PlexAlbumSummary]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return []

        viewed: list[PlexAlbum] = []
        for sid in section_ids:
            albums = await self._plex.get_recently_viewed(section_id=sid, limit=limit)
            viewed.extend(albums)

        if viewed:
            viewed.sort(key=lambda a: a.lastViewedAt, reverse=True)
            filtered = [a for a in viewed[:limit] if a.title and a.title != "Unknown"]
        else:
            added: list[PlexAlbum] = []
            for sid in section_ids:
                albums = await self._plex.get_recently_added(section_id=sid, limit=limit)
                added.extend(albums)
            added.sort(key=lambda a: a.addedAt, reverse=True)
            filtered = [a for a in added[:limit] if a.title and a.title != "Unknown"]

        summaries = await asyncio.gather(*(self._album_to_summary(a) for a in filtered))
        return list(summaries)

    async def get_recently_played(self, limit: int = 20) -> list[PlexAlbumSummary]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return []

        viewed: list[PlexAlbum] = []
        for sid in section_ids:
            albums = await self._plex.get_recently_viewed(section_id=sid, limit=limit)
            viewed.extend(albums)

        if not viewed:
            return []

        viewed.sort(key=lambda a: a.lastViewedAt, reverse=True)
        filtered = [a for a in viewed[:limit] if a.title and a.title != "Unknown"]
        summaries = await asyncio.gather(*(self._album_to_summary(a) for a in filtered))
        return list(summaries)

    async def get_recently_added_albums(self, limit: int = 20) -> list[PlexAlbumSummary]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return []

        added: list[PlexAlbum] = []
        for sid in section_ids:
            albums = await self._plex.get_recently_added(section_id=sid, limit=limit)
            added.extend(albums)

        added.sort(key=lambda a: a.addedAt, reverse=True)
        filtered = [a for a in added[:limit] if a.title and a.title != "Unknown"]
        summaries = await asyncio.gather(*(self._album_to_summary(a) for a in filtered))
        return list(summaries)

    async def get_genres(self) -> list[str]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return []

        all_genres: set[str] = set()
        for sid in section_ids:
            genres = await self._plex.get_genres(section_id=sid)
            all_genres.update(genres)

        return sorted(all_genres)

    async def get_songs_by_genre(
        self, genre: str, limit: int = 50, offset: int = 0
    ) -> tuple[list[PlexTrackInfo], int]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return [], 0
        sid = section_ids[0]
        tracks_raw, total = await self._plex.get_tracks(
            section_id=sid, size=limit, offset=offset,
            sort="titleSort:asc", genre=genre,
        )
        return [self._track_to_info(t) for t in tracks_raw], total

    async def get_moods(self) -> list[str]:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return []

        all_moods: set[str] = set()
        for sid in section_ids:
            moods = await self._plex.get_moods(section_id=sid)
            all_moods.update(moods)

        return sorted(all_moods)

    async def get_stats(self) -> PlexLibraryStats:
        stats_ttl = self._plex.stats_ttl
        if self._stats_cache is not None and (time.monotonic() - self._stats_cache_ts) < stats_ttl:
            return self._stats_cache

        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return PlexLibraryStats()

        total_albums = 0
        total_artists = 0
        total_tracks = 0

        for sid in section_ids:
            _, album_total = await self._plex.get_albums(section_id=sid, size=1, offset=0)
            total_albums += album_total

            track_total = await self._plex.get_track_count(section_id=sid)
            total_tracks += track_total

            artist_total = await self._plex.get_artist_count(section_id=sid)
            total_artists += artist_total

        result = PlexLibraryStats(
            total_tracks=total_tracks,
            total_albums=total_albums,
            total_artists=total_artists,
        )
        self._stats_cache = result
        self._stats_cache_ts = time.monotonic()
        return result

    async def get_album_match(
        self,
        album_id: str,
        album_name: str,
        artist_name: str,
    ) -> PlexAlbumMatch:
        sem = asyncio.Semaphore(_CONCURRENCY_LIMIT)

        async def _fetch_detail(rk: str) -> PlexAlbumDetail | None:
            async with sem:
                return await self.get_album_detail(rk)

        if album_id and album_id in self._mbid_to_plex_id:
            plex_id = self._mbid_to_plex_id[album_id]
            detail = await _fetch_detail(plex_id)
            if detail:
                return PlexAlbumMatch(
                    found=True,
                    plex_album_id=detail.plex_id,
                    tracks=detail.tracks,
                )

        if album_name:
            section_ids = self._get_configured_section_ids()
            candidates: list[PlexAlbum] = []
            seen: set[str] = set()
            for sid in section_ids:
                result = await self._plex.search(album_name, section_id=sid, limit=50)
                for a in result.get("albums", []):
                    if a.ratingKey not in seen:
                        seen.add(a.ratingKey)
                        candidates.append(a)

            if album_id:
                for candidate in candidates:
                    candidate_mbid = extract_mbid_from_guids(candidate.Guid)
                    if candidate_mbid and candidate_mbid == album_id:
                        detail = await _fetch_detail(candidate.ratingKey)
                        if detail:
                            return PlexAlbumMatch(
                                found=True,
                                plex_album_id=detail.plex_id,
                                tracks=detail.tracks,
                            )

            if artist_name:
                norm_album = _normalize(album_name)
                norm_artist = _normalize(artist_name)
                for candidate in candidates:
                    if (
                        _normalize(candidate.title) == norm_album
                        and _normalize(candidate.parentTitle) == norm_artist
                    ):
                        detail = await _fetch_detail(candidate.ratingKey)
                        if detail:
                            return PlexAlbumMatch(
                                found=True,
                                plex_album_id=detail.plex_id,
                                tracks=detail.tracks,
                            )

        return PlexAlbumMatch(found=False)

    async def list_playlists(self, limit: int = 50) -> list[PlexPlaylistSummary]:
        raw = await self._plex.get_playlists()
        summaries = []
        for p in raw[:limit]:
            cover = f"/api/v1/plex/playlist-thumb/{p.ratingKey}"
            summaries.append(PlexPlaylistSummary(
                id=p.ratingKey,
                name=p.title,
                track_count=p.leafCount,
                duration_seconds=p.duration // 1000 if p.duration else 0,
                is_smart=p.smart,
                cover_url=cover,
                updated_at=str(p.updatedAt) if p.updatedAt else "",
            ))
        return summaries

    async def get_playlist_detail(self, playlist_id: str) -> PlexPlaylistDetail:
        raw = await self._plex.get_playlists()
        playlist = next((p for p in raw if p.ratingKey == playlist_id), None)
        if playlist is None:
            from core.exceptions import ResourceNotFoundError
            raise ResourceNotFoundError(f"Plex playlist {playlist_id} not found")

        items = await self._plex.get_playlist_items(playlist_id)
        tracks = [
            PlexPlaylistTrack(
                id=t.ratingKey,
                track_name=t.title,
                artist_name=t.grandparentTitle,
                album_name=t.parentTitle,
                album_id=str(t.parentRatingKey) if t.parentRatingKey else "",
                plex_rating_key=t.ratingKey,
                part_key=t.Media[0].Part[0].key if (t.Media and t.Media[0].Part) else "",
                duration_seconds=t.duration // 1000 if t.duration else 0,
                track_number=t.index,
                disc_number=t.parentIndex if t.parentIndex else 1,
                cover_url=f"/api/v1/plex/thumb/{t.parentRatingKey}" if t.parentRatingKey else "",
            )
            for t in items
        ]
        cover = f"/api/v1/plex/playlist-thumb/{playlist.ratingKey}"
        return PlexPlaylistDetail(
            id=playlist.ratingKey,
            name=playlist.title,
            track_count=playlist.leafCount,
            duration_seconds=playlist.duration // 1000 if playlist.duration else 0,
            is_smart=playlist.smart,
            cover_url=cover,
            updated_at=str(playlist.updatedAt) if playlist.updatedAt else "",
            tracks=tracks,
        )

    async def import_playlist(
        self,
        playlist_id: str,
        playlist_service: 'PlaylistService',
    ) -> PlexImportResult:
        source_ref = f"plex:{playlist_id}"
        existing = await playlist_service.get_by_source_ref(source_ref)
        if existing:
            return PlexImportResult(
                musicseerr_playlist_id=existing.id,
                already_imported=True,
            )

        detail = await self.get_playlist_detail(playlist_id)
        try:
            created = await playlist_service.create_playlist(detail.name, source_ref=source_ref)
        except Exception:  # noqa: BLE001
            re_check = await playlist_service.get_by_source_ref(source_ref)
            if re_check:
                return PlexImportResult(musicseerr_playlist_id=re_check.id, already_imported=True)
            raise

        track_dicts = []
        failed = 0
        for t in detail.tracks:
            # The stream proxy keys off the Plex part key (/library/parts/...),
            # not the rating key - a track without one cannot be played.
            if not t.part_key:
                logger.warning(
                    "Skipping Plex track '%s' (rating key %s) during import: no streamable part",
                    t.track_name, t.plex_rating_key,
                )
                failed += 1
                continue
            try:
                track_dicts.append({
                    "track_name": t.track_name,
                    "artist_name": t.artist_name,
                    "album_name": t.album_name,
                    "duration": t.duration_seconds,
                    "track_source_id": t.part_key,
                    "source_type": "plex",
                    "album_id": t.album_id,
                    "plex_rating_key": t.plex_rating_key,
                    "track_number": t.track_number,
                    "disc_number": t.disc_number,
                    "cover_url": t.cover_url,
                })
            except Exception:  # noqa: BLE001
                failed += 1

        if track_dicts:
            try:
                await playlist_service.add_tracks(created.id, track_dicts)
            except Exception:  # noqa: BLE001
                logger.error("Failed to add tracks during Plex playlist import %s", playlist_id, exc_info=True)
                await playlist_service.delete_playlist(created.id)
                raise ExternalServiceError(f"Failed to import Plex playlist {playlist_id}")

        return PlexImportResult(
            musicseerr_playlist_id=created.id,
            tracks_imported=len(track_dicts),
            tracks_failed=failed,
        )

    async def get_sessions(self) -> PlexSessionsResponse:
        try:
            raw_sessions = await self._plex.get_sessions()
            sessions = [
                PlexSessionInfo(
                    session_id=s.session_id,
                    user_name=s.user_name,
                    track_title=s.track_title,
                    artist_name=s.artist_name,
                    album_name=s.album_name,
                    cover_url=f"/api/v1/plex/thumb/{s.album_thumb}" if s.album_thumb else "",
                    player_device=s.player_device,
                    player_platform=s.player_platform,
                    player_state=s.player_state,
                    is_direct_play=s.is_direct_play,
                    progress_ms=s.progress_ms,
                    duration_ms=s.duration_ms,
                    audio_codec=s.audio_codec,
                    audio_channels=s.audio_channels,
                    bitrate=s.bitrate,
                )
                for s in raw_sessions
            ]
            return PlexSessionsResponse(sessions=sessions)
        except Exception:  # noqa: BLE001
            logger.warning("get_sessions failed", exc_info=True)
            return PlexSessionsResponse(sessions=[], available=False)

    async def get_discovery_hubs(self, count: int = 10) -> PlexDiscoveryResponse:
        section_ids = self._get_configured_section_ids()
        if not section_ids:
            return PlexDiscoveryResponse(hubs=[])
        try:
            raw_hubs = await self._plex.get_hubs(section_ids[0], count=count)
        except Exception:  # noqa: BLE001
            logger.warning("get_discovery_hubs failed", exc_info=True)
            return PlexDiscoveryResponse(hubs=[])
        hubs: list[PlexDiscoveryHub] = []
        for hub in raw_hubs:
            hub_type = hub.get("type", "")
            title = hub.get("title", "")
            if hub_type != "album":
                continue
            albums: list[PlexDiscoveryAlbum] = []
            for item in hub.get("Metadata", []):
                rating_key = str(item.get("ratingKey", ""))
                image_url = (
                    f"/api/v1/plex/thumb/{rating_key}" if item.get("thumb") else None
                )
                albums.append(PlexDiscoveryAlbum(
                    plex_id=rating_key,
                    name=item.get("title", ""),
                    artist_name=item.get("parentTitle", ""),
                    year=item.get("year"),
                    image_url=image_url,
                ))
            if albums:
                hubs.append(PlexDiscoveryHub(
                    title=title,
                    hub_type=hub_type,
                    albums=albums,
                ))
        return PlexDiscoveryResponse(hubs=hubs)

    async def get_hub_data(self) -> PlexHubResponse:
        _HUB_TIMEOUT = 10

        results = await asyncio.gather(
            asyncio.wait_for(self.get_recently_played(limit=20), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_albums(size=12), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_stats(), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_recently_added_albums(limit=20), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.list_playlists(limit=20), timeout=_HUB_TIMEOUT),
            asyncio.wait_for(self.get_genres(), timeout=_HUB_TIMEOUT),
            return_exceptions=True,
        )

        all_failed = all(isinstance(r, BaseException) for r in results)
        if all_failed:
            raise ExternalServiceError("All Plex hub data requests failed")

        recently_played = results[0] if not isinstance(results[0], BaseException) else []
        if isinstance(results[0], BaseException):
            logger.warning("Hub: get_recently_played failed: %s", results[0])

        albums_result = results[1]
        if isinstance(albums_result, BaseException):
            logger.warning("Hub: get_albums failed: %s", albums_result)
            all_albums_preview: list[PlexAlbumSummary] = []
        else:
            all_albums_preview = albums_result[0]

        stats = results[2] if not isinstance(results[2], BaseException) else None
        if isinstance(results[2], BaseException):
            logger.warning("Hub: get_stats failed: %s", results[2])

        recently_added = results[3] if not isinstance(results[3], BaseException) else []
        if isinstance(results[3], BaseException):
            logger.warning("Hub: get_recently_added_albums failed: %s", results[3])

        playlists = results[4] if not isinstance(results[4], BaseException) else []
        if isinstance(results[4], BaseException):
            logger.warning("Hub: list_playlists failed: %s", results[4])

        genres = results[5] if not isinstance(results[5], BaseException) else []
        if isinstance(results[5], BaseException):
            logger.warning("Hub: get_genres failed: %s", results[5])

        return PlexHubResponse(
            stats=stats,
            recently_played=recently_played,
            recently_added=recently_added,
            all_albums_preview=all_albums_preview,
            playlists=playlists,
            genres=genres,
        )

    async def warm_mbid_cache(self) -> None:
        if self._library_db:
            try:
                lidarr_albums = await self._library_db.get_all_albums_for_matching()
                self._lidarr_album_index = {}
                self._lidarr_artist_index = {}
                for title, artist_name, album_mbid, artist_mbid in lidarr_albums:
                    key = f"{_normalize(title)}:{_normalize(artist_name)}"
                    clean_key = f"{_normalize(_clean_album_name(title))}:{_normalize(artist_name)}"
                    self._lidarr_album_index[key] = (album_mbid, artist_mbid)
                    if clean_key != key:
                        self._lidarr_album_index[clean_key] = (album_mbid, artist_mbid)
                    norm_artist = _normalize(artist_name)
                    if norm_artist and artist_mbid:
                        self._lidarr_artist_index[norm_artist] = artist_mbid
                logger.info(
                    "Built Plex Lidarr matching indices: %d album entries, %d artist entries",
                    len(self._lidarr_album_index), len(self._lidarr_artist_index),
                )
            except Exception:  # noqa: BLE001
                logger.warning("Failed to build Plex Lidarr matching indices", exc_info=True)

        if self._mbid_store:
            try:
                disk_albums = await self._mbid_store.load_plex_album_mbid_index(max_age_seconds=86400)
                disk_artists = await self._mbid_store.load_plex_artist_mbid_index(max_age_seconds=86400)
                if disk_albums or disk_artists:
                    self._album_mbid_cache.update(disk_albums)
                    self._artist_mbid_cache.update(disk_artists)
                    logger.info(
                        "Loaded Plex MBID cache from disk: %d albums, %d artists",
                        len(disk_albums), len(disk_artists),
                    )
            except Exception:  # noqa: BLE001
                logger.warning("Failed to load Plex MBID cache from disk", exc_info=True)

    async def persist_if_dirty(self) -> None:
        if not self._dirty or not self._mbid_store:
            return
        try:
            serializable_albums = {k: (v if isinstance(v, str) else None) for k, v in self._album_mbid_cache.items()}
            serializable_artists = {k: (v if isinstance(v, str) else None) for k, v in self._artist_mbid_cache.items()}
            await self._mbid_store.save_plex_album_mbid_index(serializable_albums)
            await self._mbid_store.save_plex_artist_mbid_index(serializable_artists)
            self._dirty = False
            logger.debug("Persisted dirty Plex MBID cache to disk")
        except Exception:  # noqa: BLE001
            logger.warning("Failed to persist dirty Plex MBID cache", exc_info=True)

    async def get_history(
        self, limit: int = 50, offset: int = 0
    ) -> PlexHistoryResponse:
        try:
            entries, total = await self._plex.get_listening_history(limit=limit, offset=offset)
        except Exception:  # noqa: BLE001
            logger.warning("get_history failed", exc_info=True)
            return PlexHistoryResponse(available=False)
        items = [
            PlexHistoryEntrySchema(
                rating_key=e.rating_key,
                track_title=e.track_title,
                artist_name=e.artist_name,
                album_name=e.album_name,
                cover_url=f"/api/v1/plex/thumb/{e.album_rating_key}" if e.album_rating_key else "",
                viewed_at=str(e.viewed_at),
                device_name=e.device_name,
            )
            for e in entries
        ]
        return PlexHistoryResponse(
            entries=items,
            total=total,
            limit=limit,
            offset=offset,
        )

    async def get_analytics(self) -> PlexAnalyticsResponse:
        from collections import Counter
        import time as _time

        if self._analytics_cache is not None and (_time.monotonic() - self._analytics_cache_ts) < 300:
            return self._analytics_cache

        max_entries = 5000
        all_entries = []
        offset = 0
        batch_size = 500
        total_available = 0
        deadline = _time.monotonic() + 30

        while len(all_entries) < max_entries:
            if _time.monotonic() > deadline:
                break
            entries, total = await self._plex.get_listening_history(
                limit=batch_size, offset=offset
            )
            if total > 0:
                total_available = total
            if not entries:
                break
            all_entries.extend(entries)
            offset += batch_size
            if offset >= total:
                break

        is_complete = len(all_entries) >= total_available or total_available <= max_entries

        now = _time.time()
        seven_days_ago = now - (7 * 86400)
        thirty_days_ago = now - (30 * 86400)

        artist_counts: Counter[str] = Counter()
        album_counts: Counter[tuple[str, str]] = Counter()
        track_counts: Counter[tuple[str, str]] = Counter()
        total_ms = 0
        last_7 = 0
        last_30 = 0

        for e in all_entries:
            artist_counts[e.artist_name] += 1
            album_counts[(e.album_name, e.artist_name)] += 1
            track_counts[(e.track_title, e.artist_name)] += 1
            total_ms += e.duration_ms

            try:
                viewed_ts = int(e.viewed_at)
            except (ValueError, TypeError):
                continue
            if viewed_ts >= seven_days_ago:
                last_7 += 1
            if viewed_ts >= thirty_days_ago:
                last_30 += 1

        top_artists = [
            PlexAnalyticsItem(name=name, play_count=count)
            for name, count in artist_counts.most_common(10)
        ]
        top_albums = [
            PlexAnalyticsItem(name=name, subtitle=artist, play_count=count)
            for (name, artist), count in album_counts.most_common(10)
        ]
        top_tracks = [
            PlexAnalyticsItem(name=name, subtitle=artist, play_count=count)
            for (name, artist), count in track_counts.most_common(10)
        ]

        result = PlexAnalyticsResponse(
            top_artists=top_artists,
            top_albums=top_albums,
            top_tracks=top_tracks,
            total_listens=len(all_entries),
            listens_last_7_days=last_7,
            listens_last_30_days=last_30,
            total_hours=round(total_ms / 3_600_000, 1),
            is_complete=is_complete,
            entries_analyzed=len(all_entries),
        )
        self._analytics_cache = result
        self._analytics_cache_ts = _time.monotonic()
        return result
