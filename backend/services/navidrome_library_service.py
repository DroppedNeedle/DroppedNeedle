from __future__ import annotations

import asyncio
import logging
import time
import unicodedata
import re
from typing import TYPE_CHECKING

from api.v1.schemas.navidrome import (
    NavidromeAlbumDetail,
    NavidromeAlbumInfoSchema,
    NavidromeAlbumMatch,
    NavidromeAlbumSummary,
    NavidromeArtistIndexEntry,
    NavidromeArtistIndexResponse,
    NavidromeArtistInfoSchema,
    NavidromeArtistSummary,
    NavidromeGenreSongsResponse,
    NavidromeHubResponse,
    NavidromeImportResult,
    NavidromeLibraryStats,
    NavidromeLyricLine,
    NavidromeLyricsResponse,
    NavidromeMusicFolder,
    NavidromeNowPlayingEntrySchema,
    NavidromeNowPlayingResponse,
    NavidromePlaylistDetail,
    NavidromePlaylistCollection,
    NavidromePlaylistSummary,
    NavidromePlaylistTrack,
    NavidromeSearchResponse,
    NavidromeTrackInfo,
)
from infrastructure.cover_urls import prefer_artist_cover_url, prefer_release_group_cover_url
from infrastructure.validators import clean_lastfm_bio
from core.exceptions import (
    ExternalServiceError,
    MediaAccountRelinkRequiredError,
    NavidromeAuthError,
    ResourceNotFoundError,
)
from repositories.navidrome_models import SubsonicAlbum, SubsonicSong
from repositories.protocols import NavidromeRepositoryProtocol
from services.preferences_service import PreferencesService
from services.per_user_client_factory import MediaClientResolution, PerUserClientFactory

if TYPE_CHECKING:
    from infrastructure.persistence import LibraryDB, MBIDStore

logger = logging.getLogger(__name__)

_CONCURRENCY_LIMIT = 5
_NEGATIVE_CACHE_TTL = 4 * 60 * 60


def _cache_get_mbid(cache: dict[str, str | tuple[None, float]], key: str) -> str | None:
    """Extract MBID from cache, returning None for negative or missing entries."""
    val = cache.get(key)
    if val is None:
        return None
    if isinstance(val, str):
        return val
    return None


def _clean_album_name(name: str) -> str:
    """Strip common suffixes like '(Remastered 2009)', '[Deluxe Edition]', year prefixes, etc."""
    cleaned = name.strip()
    cleaned = re.sub(r'\s*[\(\[][^)\]]*(?:remaster|deluxe|edition|bonus|expanded|mono|stereo|anniversary)[^)\]]*[\)\]]', '', cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r'^\d{4}\s*[-–\u2014]\s*', '', cleaned)
    cleaned = re.sub(r'\s*-\s*EP$', '', cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r'\s*\[[^\]]*\]\s*$', '', cleaned)
    return cleaned.strip()


def _normalize(text: str) -> str:
    text = unicodedata.normalize("NFKD", text)
    text = text.encode("ascii", "ignore").decode("ascii")
    text = re.sub(r"[^a-z0-9]", "", text.lower())
    return text


class NavidromeLibraryService:

    def __init__(
        self,
        navidrome_repo: NavidromeRepositoryProtocol,
        preferences_service: PreferencesService,
        library_db: 'LibraryDB | None' = None,
        mbid_store: 'MBIDStore | None' = None,
        client_factory: PerUserClientFactory | None = None,
    ):
        self._navidrome = navidrome_repo
        self._preferences = preferences_service
        self._library_db = library_db
        self._mbid_store = mbid_store
        self._client_factory = client_factory
        self._album_mbid_cache: dict[str, str | tuple[None, float]] = {}
        self._artist_mbid_cache: dict[str, str | tuple[None, float]] = {}
        self._mbid_to_navidrome_id: dict[str, str] = {}
        self._library_album_index: dict[str, tuple[str, str]] = {}
        self._library_artist_index: dict[str, str] = {}
        self._dirty = False

    def is_configured(self) -> bool:
        try:
            return bool(self._navidrome.is_configured())
        except Exception:  # noqa: BLE001
            return True

    def lookup_navidrome_id(self, mbid: str) -> str | None:
        """Public accessor for the MBID-to-Navidrome album ID reverse index."""
        return self._mbid_to_navidrome_id.get(mbid)

    def invalidate_album_cache(self, album_mbid: str) -> None:
        """Remove cached entries for a specific album MBID, forcing re-lookup on next match."""
        self._mbid_to_navidrome_id.pop(album_mbid, None)
        stale_keys = [k for k, v in self._album_mbid_cache.items() if v == album_mbid]
        for key in stale_keys:
            del self._album_mbid_cache[key]
        if stale_keys:
            self._dirty = True

    async def _resolve_album_mbid(self, name: str, artist: str) -> str | None:
        """Resolve a release-group MBID for an album via Lidarr library matching."""
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

        match = self._library_album_index.get(cache_key)
        if match:
            self._album_mbid_cache[cache_key] = match[0]
            self._dirty = True
            return match[0]

        clean_key = f"{_normalize(_clean_album_name(name))}:{_normalize(artist)}"
        if clean_key != cache_key:
            match = self._library_album_index.get(clean_key)
            if match:
                self._album_mbid_cache[cache_key] = match[0]
                self._dirty = True
                return match[0]

        self._album_mbid_cache[cache_key] = (None, time.time())
        self._dirty = True
        return None

    async def _resolve_artist_mbid(self, name: str) -> str | None:
        """Resolve an artist MBID via Lidarr library matching."""
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

        match = self._library_artist_index.get(cache_key)
        if match:
            self._artist_mbid_cache[cache_key] = match
            self._dirty = True
            return match

        self._artist_mbid_cache[cache_key] = (None, time.time())
        self._dirty = True
        return None

    async def persist_if_dirty(self) -> None:
        """Persist in-memory MBID cache to SQLite if there are unsaved changes."""
        if not self._dirty or not self._mbid_store:
            return
        try:
            serializable_albums = {k: (v if isinstance(v, str) else None) for k, v in self._album_mbid_cache.items()}
            serializable_artists = {k: (v if isinstance(v, str) else None) for k, v in self._artist_mbid_cache.items()}
            await self._mbid_store.save_navidrome_album_mbid_index(serializable_albums)
            await self._mbid_store.save_navidrome_artist_mbid_index(serializable_artists)
            self._dirty = False
        except Exception:  # noqa: BLE001
            logger.warning("Failed to persist dirty Navidrome MBID cache", exc_info=True)

    async def _build_artist_summary(self, artist_data: object) -> NavidromeArtistSummary:
        """Build an artist summary, enriching MBID from Lidarr if needed."""
        name = getattr(artist_data, 'name', '')
        library_mbid = await self._resolve_artist_mbid(name) if name else None
        mbid = library_mbid or getattr(artist_data, 'musicBrainzId', None) or None
        image_url = prefer_artist_cover_url(mbid, None, size=500)
        return NavidromeArtistSummary(
            navidrome_id=artist_data.id,
            name=name,
            image_url=image_url,
            album_count=getattr(artist_data, 'albumCount', 0),
            musicbrainz_id=mbid,
        )

    def _song_to_track_info(self, song: SubsonicSong) -> NavidromeTrackInfo:
        return NavidromeTrackInfo(
            navidrome_id=song.id,
            title=song.title,
            track_number=song.track,
            disc_number=song.discNumber or 1,
            duration_seconds=float(song.duration),
            album_name=song.album,
            artist_name=song.artist,
            codec=song.suffix or None,
            bitrate=song.bitRate or None,
            image_url=f"/api/v1/navidrome/cover/{song.albumId}" if song.albumId else None,
        )

    async def _album_to_summary(self, album: SubsonicAlbum) -> NavidromeAlbumSummary:
        mbid = await self._resolve_album_mbid(album.name, album.artist) if album.name and album.artist else None
        if mbid:
            self._mbid_to_navidrome_id[mbid] = album.id
        artist_mbid = await self._resolve_artist_mbid(album.artist) if album.artist else None
        fallback = f"/api/v1/navidrome/cover/{album.coverArt}" if album.coverArt else None
        image_url = prefer_release_group_cover_url(mbid, fallback, size=500)
        return NavidromeAlbumSummary(
            navidrome_id=album.id,
            name=album.name,
            artist_name=album.artist,
            year=album.year or None,
            track_count=album.songCount,
            image_url=image_url,
            musicbrainz_id=mbid,
            artist_musicbrainz_id=artist_mbid,
        )

    @staticmethod
    def _fix_missing_track_numbers(tracks: list[NavidromeTrackInfo]) -> list[NavidromeTrackInfo]:
        if len(tracks) <= 1:
            return tracks
        tracks_by_disc: dict[int, list[NavidromeTrackInfo]] = {}
        for track in tracks:
            tracks_by_disc.setdefault(track.disc_number, []).append(track)

        renumbered_ids: dict[str, int] = {}
        for disc_tracks in tracks_by_disc.values():
            numbers = {t.track_number for t in disc_tracks}
            if len(numbers) > 1:
                continue
            for i, track in enumerate(disc_tracks, start=1):
                renumbered_ids[track.navidrome_id] = i

        fixed: list[NavidromeTrackInfo] = []
        for track in tracks:
            track_number = renumbered_ids.get(track.navidrome_id, track.track_number)
            fixed.append(NavidromeTrackInfo(
                navidrome_id=track.navidrome_id,
                title=track.title,
                track_number=track_number,
                disc_number=track.disc_number,
                duration_seconds=track.duration_seconds,
                album_name=track.album_name,
                artist_name=track.artist_name,
                codec=track.codec,
                bitrate=track.bitrate,
            ))
        return fixed

    async def get_albums(
        self,
        type: str = "alphabeticalByName",
        size: int = 50,
        offset: int = 0,
        genre: str | None = None,
        from_year: int | None = None,
        to_year: int | None = None,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> list[NavidromeAlbumSummary]:
        albums = await self._navidrome.get_album_list(
            type=type, size=size, offset=offset, genre=genre,
            from_year=from_year, to_year=to_year,
            music_folder_ids=music_folder_ids,
        )
        filtered = [a for a in albums if a.name and a.name != "Unknown"]
        summaries = await asyncio.gather(*(self._album_to_summary(a) for a in filtered))
        return list(summaries)

    async def get_album_detail(
        self,
        album_id: str,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromeAlbumDetail | None:
        try:
            album = await self._navidrome.get_album(album_id)
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch Navidrome album %s", album_id, exc_info=True)
            return None
        if music_folder_ids is not None:
            if not music_folder_ids:
                return None
            scoped = await self._navidrome.search(
                album.name,
                artist_count=0,
                album_count=500,
                song_count=0,
                music_folder_ids=music_folder_ids,
            )
            if not any(candidate.id == album_id for candidate in scoped.album):
                return None

        songs = album.song or []
        tracks = self._fix_missing_track_numbers(
            [self._song_to_track_info(s) for s in songs]
        )
        mbid = await self._resolve_album_mbid(album.name, album.artist) if album.name and album.artist else None
        artist_mbid = await self._resolve_artist_mbid(album.artist) if album.artist else None
        fallback = f"/api/v1/navidrome/cover/{album.coverArt}" if album.coverArt else None
        image_url = prefer_release_group_cover_url(mbid, fallback, size=500)

        return NavidromeAlbumDetail(
            navidrome_id=album.id,
            name=album.name,
            artist_name=album.artist,
            year=album.year or None,
            track_count=len(tracks),
            image_url=image_url,
            musicbrainz_id=mbid,
            artist_musicbrainz_id=artist_mbid,
            tracks=tracks,
        )

    async def get_artists(
        self, music_folder_ids: tuple[str, ...] | None = None
    ) -> list[NavidromeArtistSummary]:
        artists = await self._navidrome.get_artists(music_folder_ids)
        summaries = await asyncio.gather(*(self._build_artist_summary(a) for a in artists))
        return list(summaries)

    async def browse_artists(
        self,
        size: int = 48,
        offset: int = 0,
        search: str = "",
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> tuple[list[NavidromeArtistSummary], int]:
        all_artists = await self._navidrome.get_artists(music_folder_ids)
        if search:
            query = search.lower()
            all_artists = [a for a in all_artists if query in a.name.lower()]
        total = len(all_artists)
        page = all_artists[offset : offset + size]
        summaries = await asyncio.gather(*(self._build_artist_summary(a) for a in page))
        return list(summaries), total

    async def browse_tracks(
        self,
        size: int = 48,
        offset: int = 0,
        search: str = "",
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> tuple[list[NavidromeTrackInfo], int]:
        songs = await self._navidrome.search_songs(
            query=search,
            count=size,
            offset=offset,
            music_folder_ids=music_folder_ids,
        )
        tracks = [self._song_to_track_info(s) for s in songs]
        try:
            stats = await self.get_stats(music_folder_ids)
            total = stats.total_tracks if len(tracks) >= size else offset + len(tracks)
        except Exception:  # noqa: BLE001
            total = offset + len(tracks) + (1 if len(tracks) >= size else 0)
        return tracks, total

    async def get_artist_detail(
        self,
        artist_id: str,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> dict[str, object] | None:
        if music_folder_ids is None:
            try:
                artist = await self._navidrome.get_artist(artist_id)
            except Exception:  # noqa: BLE001
                logger.warning(
                    "Failed to fetch Navidrome artist %s", artist_id, exc_info=True
                )
                return None
        else:
            if not music_folder_ids:
                return None
            artist = next(
                (
                    candidate
                    for candidate in await self._navidrome.get_artists(
                        music_folder_ids
                    )
                    if candidate.id == artist_id
                ),
                None,
            )
            if artist is None:
                return None

        library_mbid = await self._resolve_artist_mbid(artist.name) if artist.name else None
        mbid = library_mbid or artist.musicBrainzId or None
        image_url = prefer_artist_cover_url(mbid, None, size=500)

        albums: list[NavidromeAlbumSummary] = []
        sem = asyncio.Semaphore(_CONCURRENCY_LIMIT)

        async def _fetch_album(album_id: str) -> NavidromeAlbumSummary | None:
            async with sem:
                try:
                    detail = await self._navidrome.get_album(album_id)
                    return await self._album_to_summary(detail)
                except Exception:  # noqa: BLE001
                    return None

        search_result = await self._navidrome.search(
            artist.name,
            artist_count=0,
            album_count=500,
            song_count=0,
            music_folder_ids=music_folder_ids,
        )
        artist_album_ids = [a.id for a in search_result.album if a.artistId == artist_id and a.name and a.name != "Unknown"]

        if artist_album_ids:
            fetched = await asyncio.gather(*(_fetch_album(aid) for aid in artist_album_ids))
            albums = [a for a in fetched if a is not None]

        return {
            "artist": NavidromeArtistSummary(
                navidrome_id=artist.id,
                name=artist.name,
                image_url=image_url,
                album_count=artist.albumCount,
                musicbrainz_id=mbid,
            ),
            "albums": albums,
        }

    async def search(
        self, query: str, music_folder_ids: tuple[str, ...] | None = None
    ) -> NavidromeSearchResponse:
        result = await self._navidrome.search(
            query, music_folder_ids=music_folder_ids
        )
        filtered_albums = [a for a in result.album if a.name and a.name != "Unknown"]
        albums_task = asyncio.gather(*(self._album_to_summary(a) for a in filtered_albums))
        artists_task = asyncio.gather(*(self._build_artist_summary(a) for a in result.artist))
        albums, artists = await asyncio.gather(albums_task, artists_task)
        tracks = [self._song_to_track_info(s) for s in result.song]
        return NavidromeSearchResponse(albums=list(albums), artists=list(artists), tracks=tracks)

    async def get_recent(
        self,
        limit: int = 20,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> list[NavidromeAlbumSummary]:
        albums = await self._navidrome.get_album_list(
            type="recent",
            size=limit,
            offset=0,
            music_folder_ids=music_folder_ids,
        )
        filtered = [a for a in albums if a.name and a.name != "Unknown"]
        summaries = await asyncio.gather(*(self._album_to_summary(a) for a in filtered))
        return list(summaries)

    async def get_favorites(
        self, music_folder_ids: tuple[str, ...] | None = None
    ) -> NavidromeSearchResponse:
        starred = await self._navidrome.get_starred(music_folder_ids)
        filtered_albums = [a for a in starred.album if a.name and a.name != "Unknown"]
        albums_task = asyncio.gather(*(self._album_to_summary(a) for a in filtered_albums))
        artists_task = asyncio.gather(*(self._build_artist_summary(a) for a in starred.artist))
        albums, artists = await asyncio.gather(albums_task, artists_task)
        tracks = [self._song_to_track_info(s) for s in starred.song]
        return NavidromeSearchResponse(albums=list(albums), artists=list(artists), tracks=tracks)

    async def get_genres(
        self, music_folder_ids: tuple[str, ...] | None = None
    ) -> list[str]:
        if music_folder_ids is None:
            genres = await self._navidrome.get_genres()
            return [g.name for g in genres if g.name]
        if not music_folder_ids:
            return []
        names: set[str] = set()
        offset = 0
        while offset < 100_000:
            albums = await self._navidrome.get_album_list(
                type="alphabeticalByName",
                size=500,
                offset=offset,
                music_folder_ids=music_folder_ids,
            )
            names.update(album.genre for album in albums if album.genre)
            if len(albums) < 500:
                break
            offset += len(albums)
        return sorted(names, key=str.casefold)

    async def get_artists_index(
        self, music_folder_ids: tuple[str, ...] | None = None
    ) -> NavidromeArtistIndexResponse:
        index_data = await self._navidrome.get_artists_index(music_folder_ids)
        entries: list[NavidromeArtistIndexEntry] = []
        for idx in index_data:
            artists = []
            for a in idx.artists:
                mbid = a.musicBrainzId or None
                fallback = f"/api/v1/navidrome/cover/{a.coverArt}" if a.coverArt else None
                image_url = prefer_artist_cover_url(mbid, fallback, size=300) if (mbid or fallback) else None
                artists.append(NavidromeArtistSummary(
                    navidrome_id=a.id,
                    name=a.name,
                    album_count=a.albumCount,
                    image_url=image_url,
                    musicbrainz_id=mbid,
                ))
            entries.append(NavidromeArtistIndexEntry(name=idx.name, artists=artists))
        return NavidromeArtistIndexResponse(index=entries)

    async def get_songs_by_genre(
        self,
        genre: str,
        count: int = 50,
        offset: int = 0,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromeGenreSongsResponse:
        songs = await self._navidrome.get_songs_by_genre(
            genre=genre,
            count=count,
            offset=offset,
            music_folder_ids=music_folder_ids,
        )
        tracks = [self._song_to_track_info(s) for s in songs]
        return NavidromeGenreSongsResponse(songs=tracks, genre=genre)

    async def get_songs_by_genres(
        self,
        genres: list[str],
        count: int = 50,
        offset: int = 0,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromeGenreSongsResponse:
        import asyncio
        capped = genres[:10]
        per_genre = max(count // len(capped), 10)
        tasks = [
            self._navidrome.get_songs_by_genre(
                genre=g,
                count=per_genre,
                offset=offset,
                music_folder_ids=music_folder_ids,
            )
            for g in capped
        ]
        results = await asyncio.gather(*tasks)
        seen: set[str] = set()
        merged: list[NavidromeTrackInfo] = []
        for songs in results:
            for s in songs:
                if s.id not in seen:
                    seen.add(s.id)
                    merged.append(self._song_to_track_info(s))
        merged = merged[:count]
        return NavidromeGenreSongsResponse(songs=merged, genre=",".join(capped))

    async def get_music_folders(self) -> list[NavidromeMusicFolder]:
        folders = await self._navidrome.get_music_folders()
        return [NavidromeMusicFolder(id=f.id, name=f.name) for f in folders]

    async def get_stats(
        self, music_folder_ids: tuple[str, ...] | None = None
    ) -> NavidromeLibraryStats:
        artists = await self._navidrome.get_artists(music_folder_ids)
        first_page = await self._navidrome.get_album_list(
            type="alphabeticalByName",
            size=1,
            offset=0,
            music_folder_ids=music_folder_ids,
        )
        total_albums = 0
        all_albums: list = []
        if first_page:
            # Copy: the repository may hand back its cached list object, so
            # extending in place below would corrupt the cache entry and grow
            # every subsequent call (issue #371).
            all_albums = list(
                await self._navidrome.get_album_list(
                    type="alphabeticalByName",
                    size=500,
                    offset=0,
                    music_folder_ids=music_folder_ids,
                )
            )
            total_albums = len(all_albums)
            if total_albums >= 500:
                offset = 500
                while True:
                    batch = await self._navidrome.get_album_list(
                        type="alphabeticalByName",
                        size=500,
                        offset=offset,
                        music_folder_ids=music_folder_ids,
                    )
                    if not batch:
                        break
                    all_albums.extend(batch)
                    total_albums += len(batch)
                    if len(batch) < 500:
                        break
                    offset += 500
        total_songs = sum(a.songCount for a in all_albums)
        return NavidromeLibraryStats(
            total_tracks=total_songs,
            total_albums=total_albums,
            total_artists=len(artists),
        )

    async def get_album_match(
        self,
        album_id: str,
        album_name: str,
        artist_name: str,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromeAlbumMatch:
        sem = asyncio.Semaphore(_CONCURRENCY_LIMIT)

        async def _fetch_detail(aid: str) -> NavidromeAlbumDetail | None:
            async with sem:
                return await self.get_album_detail(aid)

        if album_id and album_id in self._mbid_to_navidrome_id:
            nav_id = self._mbid_to_navidrome_id[album_id]
            in_scope = music_folder_ids is None
            if music_folder_ids:
                offset = 0
                while offset < 100_000:
                    batch = await self._navidrome.get_album_list(
                        type="alphabeticalByName",
                        size=500,
                        offset=offset,
                        music_folder_ids=music_folder_ids,
                    )
                    if any(candidate.id == nav_id for candidate in batch):
                        in_scope = True
                        break
                    if len(batch) < 500:
                        break
                    offset += len(batch)
            if in_scope:
                detail = await _fetch_detail(nav_id)
                if detail:
                    return NavidromeAlbumMatch(
                        found=True,
                        navidrome_album_id=detail.navidrome_id,
                        tracks=detail.tracks,
                    )

        if album_id:
            search_result = await self._navidrome.search(
                album_name,
                artist_count=0,
                album_count=50,
                song_count=0,
                music_folder_ids=music_folder_ids,
            )
            for candidate in search_result.album:
                if candidate.musicBrainzId and candidate.musicBrainzId == album_id:
                    detail = await _fetch_detail(candidate.id)
                    if detail:
                        return NavidromeAlbumMatch(
                            found=True,
                            navidrome_album_id=detail.navidrome_id,
                            tracks=detail.tracks,
                        )

        if album_name and artist_name:
            norm_album = _normalize(album_name)
            norm_artist = _normalize(artist_name)

            search_result = await self._navidrome.search(
                album_name,
                artist_count=0,
                album_count=50,
                song_count=0,
                music_folder_ids=music_folder_ids,
            )
            for candidate in search_result.album:
                if (
                    _normalize(candidate.name) == norm_album
                    and _normalize(candidate.artist) == norm_artist
                ):
                    detail = await _fetch_detail(candidate.id)
                    if detail:
                        return NavidromeAlbumMatch(
                            found=True,
                            navidrome_album_id=detail.navidrome_id,
                            tracks=detail.tracks,
                        )

        return NavidromeAlbumMatch(found=False)

    async def _playlist_resolution(
        self, user_id: str
    ) -> MediaClientResolution[NavidromeRepositoryProtocol]:
        if self._client_factory is not None:
            linked = await self._client_factory.resolve_navidrome_playlist(user_id)
            if linked is not None:
                return linked
        return MediaClientResolution(
            repository=self._navidrome,
            account_mode="shared",
            account_label="Shared Navidrome account",
            cache_scope="shared",
        )

    async def _scoped_song_ids(
        self,
        repo: NavidromeRepositoryProtocol,
        music_folder_ids: tuple[str, ...],
    ) -> set[str]:
        if not music_folder_ids:
            return set()
        ids: set[str] = set()
        offset = 0
        while offset < 100_000:
            songs = await repo.search_songs(
                query="",
                count=500,
                offset=offset,
                music_folder_ids=music_folder_ids,
            )
            ids.update(song.id for song in songs)
            if len(songs) < 500:
                break
            offset += len(songs)
        return ids

    async def _list_playlists(
        self,
        repo: NavidromeRepositoryProtocol,
        limit: int = 50,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> list[NavidromePlaylistSummary]:
        raw = await repo.get_playlists()
        allowed_ids = (
            None
            if music_folder_ids is None
            else await self._scoped_song_ids(repo, music_folder_ids)
        )
        summaries = []
        for p in raw[:limit]:
            song_count = p.songCount
            duration = p.duration
            if allowed_ids is not None:
                detail = await repo.get_playlist(p.id)
                entries = [
                    song for song in (detail.entry or []) if song.id in allowed_ids
                ]
                song_count = len(entries)
                duration = sum(song.duration for song in entries)
            summaries.append(NavidromePlaylistSummary(
                id=p.id,
                name=p.name,
                track_count=song_count,
                duration_seconds=duration,
                cover_url=(
                    f"/api/v1/navidrome/playlist-cover/{p.id}/{p.coverArt or p.id}"
                    if p.id
                    else ""
                ),
                owner=p.owner,
                is_public=p.public,
                updated_at=p.changed,
            ))
        return summaries

    async def list_playlists(
        self,
        limit: int = 50,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> list[NavidromePlaylistSummary]:
        return await self._list_playlists(
            self._navidrome, limit, music_folder_ids
        )

    async def list_user_playlists(
        self,
        requesting: 'UserRecord',
        playlist_service: 'PlaylistService',
        limit: int = 50,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromePlaylistCollection:
        resolution = await self._playlist_resolution(requesting.id)
        try:
            playlists = await self._list_playlists(
                resolution.repository, limit, music_folder_ids
            )
        except NavidromeAuthError as exc:
            if resolution.account_mode == "linked":
                raise MediaAccountRelinkRequiredError(
                    "Reconnect Navidrome to check your playlists"
                ) from exc
            raise
        imported_ids = await playlist_service.get_imported_source_ids(
            "navidrome:", user_id=requesting.id
        )
        for playlist in playlists:
            playlist.is_imported = playlist.id in imported_ids
        return NavidromePlaylistCollection(
            account_mode=resolution.account_mode,
            account_label=resolution.account_label,
            playlists=playlists,
        )

    async def _get_playlist_detail(
        self,
        repo: NavidromeRepositoryProtocol,
        playlist_id: str,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromePlaylistDetail:
        visible = await repo.get_playlists()
        if not any(playlist.id == playlist_id for playlist in visible):
            raise ResourceNotFoundError("Navidrome playlist not found")
        raw = await repo.get_playlist(playlist_id)
        if raw is None:
            raise ResourceNotFoundError("Navidrome playlist not found")

        entries = raw.entry or []
        if music_folder_ids is not None:
            allowed_ids = await self._scoped_song_ids(repo, music_folder_ids)
            entries = [song for song in entries if song.id in allowed_ids]
        tracks = []
        for s in entries:
            tracks.append(NavidromePlaylistTrack(
                id=s.id,
                track_name=s.title,
                artist_name=s.artist,
                album_name=s.album,
                album_id=s.albumId,
                artist_id=s.artistId,
                duration_seconds=s.duration,
                track_number=s.track,
                disc_number=s.discNumber,
                cover_url=(
                    f"/api/v1/navidrome/playlist-cover/{playlist_id}/"
                    f"{s.coverArt or s.albumId}"
                    if (s.coverArt or s.albumId)
                    else ""
                ),
            ))

        return NavidromePlaylistDetail(
            id=raw.id,
            name=raw.name,
            track_count=len(tracks),
            duration_seconds=sum(track.duration_seconds for track in tracks),
            cover_url=(
                f"/api/v1/navidrome/playlist-cover/{raw.id}/{raw.coverArt or raw.id}"
                if raw.id
                else ""
            ),
            tracks=tracks,
        )

    async def get_playlist_detail(
        self,
        playlist_id: str,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromePlaylistDetail:
        return await self._get_playlist_detail(
            self._navidrome, playlist_id, music_folder_ids
        )

    async def get_user_playlist_detail(
        self,
        playlist_id: str,
        requesting: 'UserRecord',
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromePlaylistDetail:
        resolution = await self._playlist_resolution(requesting.id)
        try:
            return await self._get_playlist_detail(
                resolution.repository, playlist_id, music_folder_ids
            )
        except NavidromeAuthError as exc:
            if resolution.account_mode == "linked":
                raise MediaAccountRelinkRequiredError(
                    "Reconnect Navidrome to check your playlists"
                ) from exc
            raise

    async def get_playlist_cover(
        self,
        playlist_id: str,
        cover_art_id: str,
        requesting: 'UserRecord',
        size: int,
    ) -> tuple[bytes, str]:
        resolution = await self._playlist_resolution(requesting.id)
        repo = resolution.repository
        try:
            visible = await repo.get_playlists()
            summary = next(
                (playlist for playlist in visible if playlist.id == playlist_id),
                None,
            )
            if summary is None:
                raise ResourceNotFoundError("Navidrome playlist cover not found")
            detail = await repo.get_playlist(playlist_id)
            allowed = {playlist_id, summary.coverArt, detail.coverArt}
            for item in detail.entry or []:
                allowed.update((item.id, item.albumId, item.coverArt))
            allowed.discard("")
            if cover_art_id not in allowed:
                raise ResourceNotFoundError("Navidrome playlist cover not found")
            return await repo.get_cover_art(cover_art_id, size)
        except NavidromeAuthError as exc:
            if resolution.account_mode == "linked":
                raise MediaAccountRelinkRequiredError(
                    "Reconnect Navidrome to check your playlists"
                ) from exc
            raise

    async def import_playlist(
        self,
        playlist_id: str,
        playlist_service: 'PlaylistService',
        requesting: 'UserRecord',
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromeImportResult:
        source_ref = f"navidrome:{playlist_id}"
        existing = await playlist_service.get_by_source_ref(source_ref, user_id=requesting.id)
        if existing:
            return NavidromeImportResult(
                droppedneedle_playlist_id=existing.id,
                already_imported=True,
            )

        detail = await self.get_user_playlist_detail(
            playlist_id, requesting, music_folder_ids
        )
        try:
            created = await playlist_service.create_playlist(
                detail.name, source_ref=source_ref, user_id=requesting.id,
            )
        except Exception:  # noqa: BLE001
            re_check = await playlist_service.get_by_source_ref(source_ref, user_id=requesting.id)
            if re_check:
                return NavidromeImportResult(droppedneedle_playlist_id=re_check.id, already_imported=True)
            raise

        track_dicts = []
        failed = 0
        for t in detail.tracks:
            try:
                track_dicts.append({
                    "track_name": t.track_name,
                    "artist_name": t.artist_name,
                    "album_name": t.album_name,
                    "duration": t.duration_seconds,
                    "track_source_id": t.id,
                    "source_type": "navidrome",
                    "album_id": t.album_id,
                    "artist_id": t.artist_id,
                    "track_number": t.track_number,
                    "disc_number": t.disc_number,
                    "cover_url": t.cover_url,
                })
            except Exception:  # noqa: BLE001
                failed += 1

        if track_dicts:
            try:
                await playlist_service.add_tracks(created.id, requesting, track_dicts)
            except Exception:  # noqa: BLE001
                logger.error("Failed to add tracks during Navidrome playlist import %s", playlist_id, exc_info=True)
                await playlist_service.delete_playlist(created.id, requesting)
                raise ExternalServiceError(f"Failed to import Navidrome playlist {playlist_id}")

        return NavidromeImportResult(
            droppedneedle_playlist_id=created.id,
            tracks_imported=len(track_dicts),
            tracks_failed=failed,
        )

    async def get_random_songs(
        self,
        size: int = 20,
        genre: str | None = None,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> list[NavidromeTrackInfo]:
        try:
            songs = await self._navidrome.get_random_songs(
                size=size,
                genre=genre,
                music_folder_ids=music_folder_ids,
            )
            return [self._song_to_track_info(s) for s in songs]
        except Exception:  # noqa: BLE001
            logger.warning("get_random_songs failed", exc_info=True)
            return []

    async def get_now_playing(self) -> NavidromeNowPlayingResponse:
        from services.navidrome_playback_service import NavidromePlaybackService

        try:
            entries = await self._navidrome.get_now_playing()
            mapped = [
                NavidromeNowPlayingEntrySchema(
                    user_name=e.username,
                    minutes_ago=e.minutesAgo,
                    player_name=e.playerName,
                    track_name=e.title,
                    artist_name=e.artist,
                    album_name=e.album,
                    album_id=e.albumId,
                    cover_art_id=e.coverArt,
                    duration_seconds=e.duration,
                    estimated_position_seconds=NavidromePlaybackService.get_estimated_position(e.id) or 0.0,
                )
                for e in entries
            ]
            return NavidromeNowPlayingResponse(entries=mapped)
        except Exception:  # noqa: BLE001
            logger.warning("get_now_playing failed", exc_info=True)
            return NavidromeNowPlayingResponse(entries=[])

    async def get_hub_data(
        self, music_folder_ids: tuple[str, ...] | None = None
    ) -> NavidromeHubResponse:
        _HUB_TIMEOUT = 10

        results = await asyncio.gather(
            asyncio.wait_for(
                self.get_recent(limit=20, music_folder_ids=music_folder_ids),
                timeout=_HUB_TIMEOUT,
            ),
            asyncio.wait_for(
                self.get_favorites(music_folder_ids), timeout=_HUB_TIMEOUT
            ),
            asyncio.wait_for(
                self.get_albums(size=12, music_folder_ids=music_folder_ids),
                timeout=_HUB_TIMEOUT,
            ),
            asyncio.wait_for(
                self.get_stats(music_folder_ids), timeout=_HUB_TIMEOUT
            ),
            asyncio.wait_for(
                self.get_genres(music_folder_ids), timeout=_HUB_TIMEOUT
            ),
            return_exceptions=True,
        )

        all_failed = all(isinstance(r, BaseException) for r in results)
        if all_failed:
            raise ExternalServiceError("All Navidrome hub data requests failed")

        recently_played = results[0] if not isinstance(results[0], BaseException) else []
        if isinstance(results[0], BaseException):
            logger.warning("Hub: get_recent failed: %s", results[0])

        favorites_result = results[1]
        if isinstance(favorites_result, BaseException):
            logger.warning("Hub: get_favorites failed: %s", favorites_result)
            favorites: list[NavidromeAlbumSummary] = []
            favorite_artists: list[NavidromeArtistSummary] = []
            favorite_tracks: list[NavidromeTrackInfo] = []
        else:
            favorites = favorites_result.albums
            favorite_artists = favorites_result.artists
            favorite_tracks = favorites_result.tracks

        all_albums_preview = results[2] if not isinstance(results[2], BaseException) else []
        if isinstance(results[2], BaseException):
            logger.warning("Hub: get_albums failed: %s", results[2])

        stats = results[3] if not isinstance(results[3], BaseException) else None
        if isinstance(results[3], BaseException):
            logger.warning("Hub: get_stats failed: %s", results[3])

        genres = results[4] if not isinstance(results[4], BaseException) else []
        if isinstance(results[4], BaseException):
            logger.warning("Hub: get_genres failed: %s", results[4])

        return NavidromeHubResponse(
            stats=stats,
            recently_played=recently_played,
            favorites=favorites,
            favorite_artists=favorite_artists,
            favorite_tracks=favorite_tracks,
            all_albums_preview=all_albums_preview,
            genres=genres,
        )

    async def warm_mbid_cache(self) -> None:
        """Background task: enrich all Navidrome albums and artists with MBIDs from Lidarr library matching.
        Loads from SQLite first for instant startup; enriches from Lidarr library matching."""

        if self._library_db:
            try:
                library_albums = await self._library_db.get_all_albums_for_matching()
                self._library_album_index = {}
                self._library_artist_index = {}
                for title, artist_name, album_mbid, artist_mbid in library_albums:
                    key = f"{_normalize(title)}:{_normalize(artist_name)}"
                    clean_key = f"{_normalize(_clean_album_name(title))}:{_normalize(artist_name)}"
                    self._library_album_index[key] = (album_mbid, artist_mbid)
                    if clean_key != key:
                        self._library_album_index[clean_key] = (album_mbid, artist_mbid)
                    norm_artist = _normalize(artist_name)
                    if norm_artist and artist_mbid:
                        self._library_artist_index[norm_artist] = artist_mbid
            except Exception:  # noqa: BLE001
                logger.warning("Failed to build Lidarr matching indices", exc_info=True)

        if self._mbid_store:
            try:
                disk_albums = await self._mbid_store.load_navidrome_album_mbid_index(max_age_seconds=86400)
                disk_artists = await self._mbid_store.load_navidrome_artist_mbid_index(max_age_seconds=86400)
                if disk_albums or disk_artists:
                    self._album_mbid_cache.update(disk_albums)
                    self._artist_mbid_cache.update(disk_artists)
            except Exception:  # noqa: BLE001
                logger.warning("Failed to load Navidrome MBID cache from disk", exc_info=True)

        if not self._library_album_index:
            logger.warning("Lidarr library data unavailable - Lidarr enrichment will be skipped")

        try:
            all_albums: list[SubsonicAlbum] = []
            offset = 0
            while True:
                batch = await self._navidrome.get_album_list(
                    type="alphabeticalByName", size=500, offset=offset
                )
                if not batch:
                    break
                all_albums.extend(batch)
                if len(batch) < 500:
                    break
                offset += 500
        except Exception:  # noqa: BLE001
            logger.warning("Failed to fetch Navidrome albums for MBID enrichment")
            return

        current_album_keys: set[str] = set()
        current_artist_names: set[str] = set()
        for album in all_albums:
            if album.name and album.name != "Unknown":
                current_album_keys.add(f"{_normalize(album.name)}:{_normalize(album.artist)}")
            if album.artist:
                current_artist_names.add(album.artist)

        current_artist_keys = {_normalize(n) for n in current_artist_names}
        stale_album_keys = set(self._album_mbid_cache.keys()) - current_album_keys
        stale_artist_keys = set(self._artist_mbid_cache.keys()) - current_artist_keys
        for key in stale_album_keys:
            del self._album_mbid_cache[key]
        for key in stale_artist_keys:
            del self._artist_mbid_cache[key]

        resolved_albums = 0
        resolved_artists = 0

        if self._library_album_index:
            for album in all_albums:
                if not album.name or album.name == "Unknown":
                    continue
                cache_key = f"{_normalize(album.name)}:{_normalize(album.artist)}"
                existing = self._album_mbid_cache.get(cache_key)
                if isinstance(existing, str):
                    library_match = self._library_album_index.get(cache_key)
                    if not library_match:
                        clean_key = f"{_normalize(_clean_album_name(album.name))}:{_normalize(album.artist)}"
                        if clean_key != cache_key:
                            library_match = self._library_album_index.get(clean_key)
                    if library_match and library_match[0] != existing:
                        self._album_mbid_cache[cache_key] = library_match[0]
                        self._dirty = True
                        resolved_albums += 1
                    continue
                if isinstance(existing, tuple):
                    library_hit = self._library_album_index.get(cache_key)
                    if not library_hit:
                        clean_key = f"{_normalize(_clean_album_name(album.name))}:{_normalize(album.artist)}"
                        if clean_key != cache_key:
                            library_hit = self._library_album_index.get(clean_key)
                    if library_hit:
                        del self._album_mbid_cache[cache_key]
                    elif time.time() - existing[1] < _NEGATIVE_CACHE_TTL:
                        continue
                mbid = await self._resolve_album_mbid(album.name, album.artist)
                if mbid:
                    resolved_albums += 1

            for name in current_artist_names:
                norm = _normalize(name)
                existing = self._artist_mbid_cache.get(norm)
                if isinstance(existing, str):
                    library_match = self._library_artist_index.get(norm)
                    if library_match and library_match != existing:
                        self._artist_mbid_cache[norm] = library_match
                        self._dirty = True
                        resolved_artists += 1
                    continue
                if isinstance(existing, tuple):
                    library_hit = self._library_artist_index.get(norm)
                    if library_hit:
                        del self._artist_mbid_cache[norm]
                    elif time.time() - existing[1] < _NEGATIVE_CACHE_TTL:
                        continue
                mbid = await self._resolve_artist_mbid(name)
                if mbid:
                    resolved_artists += 1

        if self._mbid_store and (self._dirty or stale_album_keys or stale_artist_keys):
            try:
                serializable_albums = {k: (v if isinstance(v, str) else None) for k, v in self._album_mbid_cache.items()}
                serializable_artists = {k: (v if isinstance(v, str) else None) for k, v in self._artist_mbid_cache.items()}
                await self._mbid_store.save_navidrome_album_mbid_index(serializable_albums)
                await self._mbid_store.save_navidrome_artist_mbid_index(serializable_artists)
                self._dirty = False
            except Exception:  # noqa: BLE001
                logger.warning("Failed to persist Navidrome MBID cache to disk", exc_info=True)

        self._mbid_to_navidrome_id.clear()
        for album in all_albums:
            if not album.name or album.name == "Unknown":
                continue
            cache_key = f"{_normalize(album.name)}:{_normalize(album.artist)}"
            mbid = _cache_get_mbid(self._album_mbid_cache, cache_key)
            if mbid:
                self._mbid_to_navidrome_id[mbid] = album.id

    async def get_top_songs(
        self,
        artist_name: str,
        count: int = 20,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> list[NavidromeTrackInfo]:
        if music_folder_ids is not None:
            return []
        try:
            songs = await self._navidrome.get_top_songs(artist_name, count=count)
            return [
                NavidromeTrackInfo(
                    navidrome_id=s.id,
                    title=s.title,
                    track_number=s.track,
                    duration_seconds=s.duration,
                    disc_number=s.discNumber,
                    album_name=s.album,
                    artist_name=s.artist,
                )
                for s in songs
            ]
        except Exception:  # noqa: BLE001
            logger.debug("Top songs unavailable for %s (Last.fm may not be configured)", artist_name)
            return []

    async def get_similar_songs(
        self,
        song_id: str,
        count: int = 20,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> list[NavidromeTrackInfo]:
        if music_folder_ids is not None:
            return []
        try:
            songs = await self._navidrome.get_similar_songs(song_id, count=count)
            return [
                NavidromeTrackInfo(
                    navidrome_id=s.id,
                    title=s.title,
                    track_number=s.track,
                    duration_seconds=s.duration,
                    disc_number=s.discNumber,
                    album_name=s.album,
                    artist_name=s.artist,
                )
                for s in songs
            ]
        except Exception:  # noqa: BLE001
            logger.debug("Similar songs unavailable for %s (Last.fm may not be configured)", song_id)
            return []

    async def get_artist_info(
        self,
        artist_id: str,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromeArtistInfoSchema | None:
        try:
            allowed_artist_ids: set[str] | None = None
            if music_folder_ids is not None:
                allowed_artist_ids = {
                    artist.id
                    for artist in await self._navidrome.get_artists(
                        music_folder_ids
                    )
                }
                if artist_id not in allowed_artist_ids:
                    return None
            info = await self._navidrome.get_artist_info(artist_id)
            if info is None:
                return None
            artist = await self._navidrome.get_artist(artist_id)
            artist_name = artist.name if artist else ""
            similar = [
                NavidromeArtistSummary(
                    navidrome_id=a.id,
                    name=a.name,
                )
                for a in info.similarArtist
                if allowed_artist_ids is None or a.id in allowed_artist_ids
            ]
            image = ""
            if info.largeImageUrl:
                image = info.largeImageUrl
            elif info.mediumImageUrl:
                image = info.mediumImageUrl
            elif info.smallImageUrl:
                image = info.smallImageUrl
            return NavidromeArtistInfoSchema(
                navidrome_id=artist_id,
                name=artist_name,
                biography=clean_lastfm_bio(info.biography),
                image_url=image,
                similar_artists=similar,
            )
        except Exception:  # noqa: BLE001
            logger.debug("Artist info unavailable for %s (Last.fm may not be configured)", artist_id)
            return None

    async def get_album_info(
        self,
        album_id: str,
        music_folder_ids: tuple[str, ...] | None = None,
    ) -> NavidromeAlbumInfoSchema | None:
        try:
            if (
                music_folder_ids is not None
                and await self.get_album_detail(album_id, music_folder_ids) is None
            ):
                return None
            info = await self._navidrome.get_album_info(album_id)
            if info is None:
                return None
            if not info.notes and not info.musicBrainzId and not info.lastFmUrl:
                return None
            image = ""
            if info.largeImageUrl:
                image = info.largeImageUrl
            elif info.mediumImageUrl:
                image = info.mediumImageUrl
            elif info.smallImageUrl:
                image = info.smallImageUrl
            return NavidromeAlbumInfoSchema(
                album_id=album_id,
                notes=clean_lastfm_bio(info.notes),
                musicbrainz_id=info.musicBrainzId,
                lastfm_url=info.lastFmUrl,
                image_url=image,
            )
        except Exception:  # noqa: BLE001
            logger.debug("Album info unavailable for %s", album_id)
            return None

    async def get_lyrics(
        self, song_id: str, artist: str = "", title: str = ""
    ) -> NavidromeLyricsResponse | None:
        try:
            lyrics = await self._navidrome.get_lyrics_by_song_id(song_id)
            if lyrics and (lyrics.value.strip() or lyrics.lines):
                lines = [
                    NavidromeLyricLine(
                        text=l.value,
                        start_seconds=l.start / 1000.0 if l.start is not None else None,
                    )
                    for l in lyrics.lines
                ] if lyrics.lines else []
                return NavidromeLyricsResponse(
                    text=lyrics.value,
                    is_synced=lyrics.is_synced,
                    lines=lines,
                )
        except Exception:  # noqa: BLE001
            logger.debug("getLyricsBySongId fallback for %s", song_id)
        if artist and title:
            try:
                lyrics = await self._navidrome.get_lyrics(artist, title)
                if lyrics and (lyrics.value.strip() or lyrics.lines):
                    return NavidromeLyricsResponse(text=lyrics.value, is_synced=False)
            except Exception:  # noqa: BLE001
                logger.debug("getLyrics also failed for %s - %s", artist, title)
        return None
