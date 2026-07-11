import asyncio
import logging
import re
from collections import defaultdict
from difflib import SequenceMatcher
from pathlib import Path
from typing import Any, Optional

import msgspec

from core.exceptions import (
    InvalidPlaylistDataError,
    PermissionDeniedError,
    PlaylistNotFoundError,
    ResourceNotFoundError,
    SourceResolutionError,
)
from infrastructure.cache.cache_keys import SOURCE_RESOLUTION_PREFIX
from infrastructure.cache.memory_cache import CacheInterface
from infrastructure.persistence.auth_store import AuthStore, UserRecord
from repositories.async_playlist_repository import AsyncPlaylistRepository
from repositories.playlist_repository import (
    PlaylistRecord,
    PlaylistRepository,
    PlaylistSummaryRecord,
    PlaylistTrackRecord,
)

logger = logging.getLogger(__name__)


class PlaylistSummaryView(msgspec.Struct, frozen=True):
    """A list-row a user is allowed to see in full: the summary record plus the
    visibility decision (D4). ``owner_name`` is set only for non-owned public rows."""
    record: PlaylistSummaryRecord
    is_owner: bool
    owner_name: str | None = None


class RedactedSummaryView(msgspec.Struct, frozen=True):
    """A private list-row an admin may only see redacted (D4): no name, no tracks."""
    id: str
    track_count: int
    owner_name: str | None = None


class PlaylistDetailView(msgspec.Struct, frozen=True):
    """A playlist detail a user is allowed to see in full (owner or public, D4)."""
    record: PlaylistRecord
    tracks: list[PlaylistTrackRecord]
    is_owner: bool
    owner_name: str | None = None


class RedactedDetailView(msgspec.Struct, frozen=True):
    """A private playlist detail an admin may only see redacted (D4)."""
    id: str
    track_count: int
    owner_name: str | None = None

ALLOWED_IMAGE_TYPES = {"image/jpeg", "image/png", "image/webp"}
MAX_COVER_SIZE = 2 * 1024 * 1024
_MIME_TO_EXT = {"image/jpeg": ".jpg", "image/png": ".png", "image/webp": ".webp"}
_SAFE_ID_RE = re.compile(r"^[a-f0-9\-]+$")
VALID_SOURCE_TYPES = {"local", "jellyfin", "navidrome", "plex", "youtube", ""}
MAX_NAME_LENGTH = 100
# Albums in a playlist are resolved against external sources concurrently. A
# large playlist (300+ tracks) can span hundreds of albums; resolving them one
# at a time turned the synchronous /resolve-sources call into hundreds of serial
# round-trips and made big playlists appear to hang. Bound the fan-out so we
# don't hammer Navidrome/Plex/Jellyfin all at once.
_ALBUM_RESOLVE_CONCURRENCY = 8

_SOURCE_TYPE_ALIASES = {
    "local": "local",
    "howler": "local",
    "jellyfin": "jellyfin",
    "navidrome": "navidrome",
    "plex": "plex",
    "youtube": "youtube",
    "droppedneedle-local": "droppedneedle-local",  # Q11: compat-created local entry
    "": "",
}


def _normalize_source_map(by_num: dict) -> dict[tuple[int, int], tuple[str, str]]:
    """Ensure source map keys are (disc_number, track_number) tuples.

    Handles old cached entries that used bare int track_number keys (assumes disc 1),
    and list keys that JSON round-trips produce from tuples.
    """
    if not by_num:
        return by_num
    first_key = next(iter(by_num))
    if isinstance(first_key, tuple):
        return by_num
    normalized: dict[tuple[int, int], tuple[str, str]] = {}
    for k, v in by_num.items():
        try:
            if isinstance(k, (list, tuple)) and len(k) == 2:
                normalized[(int(k[0]), int(k[1]))] = v
            elif isinstance(k, int):
                normalized[(1, k)] = v
            else:
                normalized[(1, int(k))] = v
        except (TypeError, ValueError):
            continue
    return normalized


def _safe_track_number(value: object) -> int | None:
    """Coerce a track number to int, returning None for non-numeric inputs."""
    if isinstance(value, int):
        return value
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _fuzzy_name_match(name1: str, name2: str) -> bool:
    if not name1 or not name2:
        return False
    n1, n2 = name1.lower().strip(), name2.lower().strip()
    if n1 == n2:
        return True
    if n1 in n2 or n2 in n1:
        return True
    return SequenceMatcher(None, n1, n2).ratio() > 0.6


class PlaylistService:
    def __init__(
        self,
        repo: PlaylistRepository,
        cache_dir: Path,
        cache: Optional[CacheInterface] = None,
        genre_index: Any = None,
        auth_store: AuthStore | None = None,
        library_db: Any = None,
    ):
        self._repo = AsyncPlaylistRepository(repo)
        self._cover_dir = cache_dir / "covers" / "playlists"
        self._cache = cache
        self._genre_index = genre_index
        self._auth_store = auth_store
        self._library_db = library_db  # LibraryDB; needed by add_file_id_entry (Q11)

    async def _load_owned_or_raise(
        self, playlist_id: str, requesting: UserRecord,
    ) -> PlaylistRecord:
        """Load a playlist for MUTATION. Owner only (admins included cannot mutate
        another user's playlist, D4/AMU-2). Missing -> 404 (no existence leak)."""
        playlist = await self._repo.get_playlist(playlist_id)
        if playlist is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        if playlist.user_id != requesting.id:
            raise PermissionDeniedError("You do not have permission to modify this playlist")
        return playlist

    async def _owner_name(self, user_id: str | None) -> str | None:
        if not user_id or self._auth_store is None:
            return None
        user = await self._auth_store.get_user_by_id(user_id)
        return user.display_name if user is not None else None

    async def _resolve_owner_names(self, user_ids: set[str]) -> dict[str, str]:
        names: dict[str, str] = {}
        for uid in user_ids:
            name = await self._owner_name(uid)
            if name is not None:
                names[uid] = name
        return names

    async def create_playlist(
        self, name: str, *, source_ref: str | None = None, user_id: str,
    ) -> PlaylistRecord:
        stripped = name.strip() if name else ""
        if not stripped:
            raise InvalidPlaylistDataError("Playlist name must not be empty")
        if len(stripped) > MAX_NAME_LENGTH:
            raise InvalidPlaylistDataError(f"Playlist name must not exceed {MAX_NAME_LENGTH} characters")
        result = await self._repo.create_playlist(stripped, source_ref=source_ref, user_id=user_id)
        return result

    async def get_by_source_ref(
        self, source_ref: str, user_id: str | None = None,
    ) -> PlaylistRecord | None:
        return await self._repo.get_by_source_ref(source_ref, user_id)

    async def get_imported_source_ids(self, prefix: str, user_id: str | None = None) -> set[str]:
        return await self._repo.get_imported_source_ids(prefix, user_id)

    async def get_playlist(self, playlist_id: str) -> PlaylistRecord:
        result = await self._repo.get_playlist(playlist_id)
        if result is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        return result

    async def get_all_playlists(
        self, requesting: UserRecord,
    ) -> list[PlaylistSummaryView | RedactedSummaryView]:
        is_admin = requesting.role == "admin"
        rows = await self._repo.get_all_playlists(user_id=None if is_admin else requesting.id)
        owner_names = await self._resolve_owner_names(
            {r.user_id for r in rows if r.user_id and r.user_id != requesting.id}
        )
        views: list[PlaylistSummaryView | RedactedSummaryView] = []
        for r in rows:
            if r.user_id == requesting.id:
                views.append(PlaylistSummaryView(record=r, is_owner=True))
            elif r.is_public:
                views.append(PlaylistSummaryView(
                    record=r, is_owner=False, owner_name=owner_names.get(r.user_id),
                ))
            else:
                # Reached only for admins (non-admins never receive private non-owned
                # rows from the repo). Redact: keep count + owner, drop name/tracks (D4).
                views.append(RedactedSummaryView(
                    id=r.id, track_count=r.track_count, owner_name=owner_names.get(r.user_id),
                ))
        return views

    async def get_playlist_with_tracks(
        self, playlist_id: str, requesting: UserRecord,
    ) -> PlaylistDetailView | RedactedDetailView:
        playlist = await self._repo.get_playlist(playlist_id)
        if playlist is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        if playlist.user_id == requesting.id:
            tracks = await self._repo.get_tracks(playlist_id)
            return PlaylistDetailView(record=playlist, tracks=tracks, is_owner=True)
        if playlist.is_public:
            tracks = await self._repo.get_tracks(playlist_id)
            owner_name = await self._owner_name(playlist.user_id)
            return PlaylistDetailView(
                record=playlist, tracks=tracks, is_owner=False, owner_name=owner_name,
            )
        if requesting.role == "admin":
            tracks = await self._repo.get_tracks(playlist_id)
            owner_name = await self._owner_name(playlist.user_id)
            return RedactedDetailView(
                id=playlist.id, track_count=len(tracks), owner_name=owner_name,
            )
        # Private, non-owner, non-admin: do NOT leak existence (404, not 403).
        raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")

    async def update_playlist(
        self, playlist_id: str, requesting: UserRecord, name: Optional[str] = None,
    ) -> PlaylistRecord:
        await self._load_owned_or_raise(playlist_id, requesting)
        if name is not None:
            stripped = name.strip()
            if not stripped:
                raise InvalidPlaylistDataError("Playlist name must not be empty")
            if len(stripped) > MAX_NAME_LENGTH:
                raise InvalidPlaylistDataError(f"Playlist name must not exceed {MAX_NAME_LENGTH} characters")
            name = stripped

        result = await self._repo.update_playlist(playlist_id, name=name)
        if result is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        return result

    async def update_playlist_with_detail(
        self, playlist_id: str, requesting: UserRecord, name: Optional[str] = None,
    ) -> tuple[PlaylistRecord, list[PlaylistTrackRecord]]:
        playlist = await self.update_playlist(playlist_id, requesting, name=name)
        tracks = await self._repo.get_tracks(playlist_id)
        return playlist, tracks

    async def set_public(
        self, playlist_id: str, requesting: UserRecord, is_public: bool,
    ) -> PlaylistSummaryView:
        # Owner-only: an admin cannot publish another user's playlist (D4).
        await self._load_owned_or_raise(playlist_id, requesting)
        result = await self._repo.set_public(playlist_id, is_public)
        if result is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        summary = await self._repo.get_summary(playlist_id)
        if summary is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        return PlaylistSummaryView(record=summary, is_owner=True, owner_name=None)

    async def delete_playlist(self, playlist_id: str, requesting: UserRecord) -> None:
        # Owner deletes own; an admin may delete any playlist (owner-cleanup, D4).
        playlist = await self._repo.get_playlist(playlist_id)
        if playlist is None:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        if playlist.user_id != requesting.id and requesting.role != "admin":
            raise PermissionDeniedError("You do not have permission to delete this playlist")
        deleted = await self._repo.delete_playlist(playlist_id)
        if not deleted:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        await asyncio.to_thread(self._delete_cover_file, playlist_id)


    async def add_tracks(
        self,
        playlist_id: str,
        requesting: UserRecord,
        tracks: list[dict],
        position: Optional[int] = None,
    ) -> list[PlaylistTrackRecord]:
        if not tracks:
            raise InvalidPlaylistDataError("Track list must not be empty")
        normalized_tracks: list[dict] = []
        for track in tracks:
            normalized = dict(track)
            st = normalized.get("source_type", "")
            if st and st not in _SOURCE_TYPE_ALIASES:
                raise InvalidPlaylistDataError(
                    f"Invalid source_type '{st}'. Allowed: {', '.join(sorted(_SOURCE_TYPE_ALIASES.keys() - {''}))}"  # noqa: E501
                )
            normalized["source_type"] = _SOURCE_TYPE_ALIASES.get(st, st)

            sources = normalized.get("available_sources")
            if sources is not None:
                normalized_sources: list[str] = []
                for source in sources:
                    if source not in _SOURCE_TYPE_ALIASES:
                        raise InvalidPlaylistDataError(
                            f"Invalid available source '{source}'. Allowed: {', '.join(sorted(_SOURCE_TYPE_ALIASES.keys() - {''}))}"  # noqa: E501
                        )
                    normalized_sources.append(_SOURCE_TYPE_ALIASES[source])
                normalized["available_sources"] = normalized_sources

            # for local sources track_source_id IS the library_files id (same
            # assumption as update_track_source); without the link compat
            # clients see an empty playlist (#181)
            if (
                normalized["source_type"] in ("local", "droppedneedle-local")
                and normalized.get("track_source_id")
                and not normalized.get("library_file_id")
            ):
                normalized["library_file_id"] = normalized["track_source_id"]

            normalized_tracks.append(normalized)
        await self._load_owned_or_raise(playlist_id, requesting)
        result = await self._repo.add_tracks(playlist_id, normalized_tracks, position)
        return result

    async def add_file_id_entry(
        self,
        playlist_id: str,
        file_id: str,
        *,
        requesting: UserRecord,
        position: Optional[int] = None,
    ) -> PlaylistTrackRecord:
        """Add one owned library file to a playlist (the ONLY entry point the compat
        shims use to add tracks, Q11). Fills the snapshot fields from the
        library_files row, links ``library_file_id``, and tags the entry
        ``source_type='droppedneedle-local'``. Ownership is enforced by add_tracks."""
        if self._library_db is None:
            raise SourceResolutionError(
                "Library database is not configured for compat playlist entries"
            )
        row = await self._library_db.get_library_file_by_id(file_id)
        if row is None or row.get("deleted_at") is not None:
            raise ResourceNotFoundError(f"Track {file_id} not found")
        duration = row.get("duration_seconds")
        track = {
            "track_name": row.get("track_title") or "",
            "artist_name": row.get("artist_name") or "",
            "album_name": row.get("album_title") or "",
            "album_id": row.get("release_group_mbid"),
            "artist_id": row.get("artist_mbid"),
            "track_source_id": file_id,
            "cover_url": None,
            "source_type": "droppedneedle-local",
            "available_sources": ["droppedneedle-local"],
            "format": row.get("file_format") or None,
            "track_number": row.get("track_number"),
            "disc_number": row.get("disc_number"),
            "duration": round(duration) if duration else None,
            "library_file_id": file_id,
        }
        created = await self.add_tracks(playlist_id, requesting, [track], position)
        return created[0]

    async def remove_track(
        self, playlist_id: str, requesting: UserRecord, track_id: str,
    ) -> None:
        await self._load_owned_or_raise(playlist_id, requesting)
        removed = await self._repo.remove_track(playlist_id, track_id)
        if not removed:
            raise PlaylistNotFoundError(f"Track {track_id} not found in playlist {playlist_id}")

    async def remove_tracks(
        self, playlist_id: str, requesting: UserRecord, track_ids: list[str],
    ) -> int:
        if not track_ids:
            raise InvalidPlaylistDataError("No track IDs provided")
        await self._load_owned_or_raise(playlist_id, requesting)
        removed = await self._repo.remove_tracks(playlist_id, track_ids)
        if not removed:
            raise PlaylistNotFoundError(f"No matching tracks found in playlist {playlist_id}")
        return removed

    async def reorder_track(
        self, playlist_id: str, requesting: UserRecord, track_id: str, new_position: int,
    ) -> int:
        if new_position < 0:
            raise InvalidPlaylistDataError("Position must be >= 0")
        await self._load_owned_or_raise(playlist_id, requesting)
        result = await self._repo.reorder_track(playlist_id, track_id, new_position)
        if result is None:
            raise PlaylistNotFoundError(f"Track {track_id} not found in playlist {playlist_id}")
        return result

    async def update_track_source(
        self,
        playlist_id: str,
        requesting: UserRecord,
        track_id: str,
        source_type: Optional[str] = None,
        available_sources: Optional[list[str]] = None,
        jf_service: object = None,
        local_service: object = None,
        nd_service: object = None,
        plex_service: object = None,
    ) -> PlaylistTrackRecord:
        await self._load_owned_or_raise(playlist_id, requesting)
        if source_type is not None and source_type not in _SOURCE_TYPE_ALIASES:
            raise InvalidPlaylistDataError(
                f"Invalid source_type '{source_type}'. Allowed: {', '.join(sorted(_SOURCE_TYPE_ALIASES.keys() - {''}))}"  # noqa: E501
            )

        normalized_source = _SOURCE_TYPE_ALIASES.get(source_type, source_type)
        normalized_available_sources = available_sources
        if available_sources is not None:
            normalized_available_sources = []
            for source in available_sources:
                if source not in _SOURCE_TYPE_ALIASES:
                    raise InvalidPlaylistDataError(
                        f"Invalid available source '{source}'. Allowed: {', '.join(sorted(_SOURCE_TYPE_ALIASES.keys() - {''}))}"  # noqa: E501
                    )
                normalized_available_sources.append(_SOURCE_TYPE_ALIASES[source])

        new_track_source_id: Optional[str] = None
        new_plex_rating_key_resolved = False
        new_plex_rating_key: Optional[str] = None
        if normalized_source:
            current_track = await self._repo.get_track(playlist_id, track_id)
            if current_track is None:
                raise PlaylistNotFoundError(f"Track {track_id} not found in playlist {playlist_id}")
            if normalized_source != current_track.source_type:
                new_track_source_id, new_plex_rating_key = await self._resolve_new_source_id(
                    current_track, normalized_source, jf_service, local_service, nd_service,
                    plex_service,
                )
                new_plex_rating_key_resolved = True

        repo_kwargs: dict[str, Any] = {
            "track_source_id": new_track_source_id,
        }
        if new_plex_rating_key_resolved:
            repo_kwargs["plex_rating_key"] = new_plex_rating_key
        if normalized_source == "local" and new_track_source_id is not None:
            repo_kwargs["library_file_id"] = new_track_source_id

        result = await self._repo.update_track_source(
            playlist_id, track_id, normalized_source, normalized_available_sources,
            **repo_kwargs,
        )
        if result is None:
            raise PlaylistNotFoundError(f"Track {track_id} not found in playlist {playlist_id}")
        return result

    async def get_tracks(self, playlist_id: str) -> list[PlaylistTrackRecord]:
        return await self._repo.get_tracks(playlist_id)

    async def get_streamable_counts(self) -> dict[str, tuple[int, int]]:
        """Per-playlist (count, duration) over entries the compat shims can stream."""
        return await self._repo.get_streamable_counts()

    async def analyse_playlist_profile(
        self, playlist_id: str, requesting: UserRecord,
    ) -> "PlaylistProfile | None":
        from api.v1.schemas.discover import PlaylistProfile

        playlist = await self._repo.get_playlist(playlist_id)
        if playlist is None:
            return None
        # The profile exposes the playlist's artists/genres (track-derived data); only
        # the owner or a public playlist may be analysed. Private non-owners (incl
        # admins) get None -> 404, never leaking existence or contents (D4).
        if playlist.user_id != requesting.id and not playlist.is_public:
            return None

        tracks = await self._repo.get_tracks(playlist_id)
        artist_mbids = list({t.artist_id for t in tracks if t.artist_id})

        genre_distribution: dict[str, list[str]] = {}
        if artist_mbids and self._genre_index is not None:
            genre_distribution = await self._genre_index.get_genres_for_artists(artist_mbids)

        return PlaylistProfile(
            artist_mbids=artist_mbids,
            genre_distribution=genre_distribution,
            track_count=len(tracks),
        )

    async def check_track_membership(
        self, tracks: list[tuple[str, str, str]], user_id: str | None = None,
    ) -> dict[str, list[int]]:
        return await self._repo.check_track_membership(tracks, user_id)


    async def resolve_track_sources(
        self,
        playlist_id: str,
        requesting: UserRecord | None = None,
        jf_service: object = None,
        local_service: object = None,
        nd_service: object = None,
        plex_service: object = None,
    ) -> dict[str, list[str]]:
        # Ownership gates the user-triggered endpoint; the post-import background task
        # runs server-side as the importer (requesting=None) and skips the check (D2).
        if requesting is not None:
            await self._load_owned_or_raise(playlist_id, requesting)
        else:
            await self.get_playlist(playlist_id)
        tracks = await self._repo.get_tracks(playlist_id)
        if not tracks:
            return {}

        album_groups: dict[str, list[PlaylistTrackRecord]] = defaultdict(list)
        no_album_tracks: list[PlaylistTrackRecord] = []
        for t in tracks:
            if t.album_id and t.track_number is not None:
                album_groups[t.album_id].append(t)
            else:
                no_album_tracks.append(t)

        result: dict[str, list[str]] = {}
        grouped = list(album_groups.items())
        sem = asyncio.Semaphore(_ALBUM_RESOLVE_CONCURRENCY)

        async def _resolve_group(
            album_tracks: list[PlaylistTrackRecord],
        ) -> tuple[
            dict[tuple[int, int], tuple[str, str]],
            dict[tuple[int, int], tuple[str, str]],
            dict[tuple[int, int], tuple[str, str]],
            dict[tuple[int, int], tuple[str, str, str]],
        ]:
            representative = album_tracks[0]
            async with sem:
                try:
                    return await self._resolve_album_sources(
                        representative.album_id, jf_service, local_service, nd_service, plex_service,
                        album_name=representative.album_name or "",
                        artist_name=representative.artist_name or "",
                    )
                except Exception:  # noqa: BLE001
                    # Now that album groups resolve concurrently, one group failing
                    # (e.g. a cache/infra error - upstream service errors are already
                    # caught inside _resolve_album_sources) must not discard every
                    # other album's results. Degrade to "no extra sources" for this
                    # album; its tracks keep their stored available_sources.
                    logger.warning(
                        "Source resolution failed for album %s; skipping",
                        representative.album_id, exc_info=True,
                    )
                    return ({}, {}, {}, {})

        resolved_maps = await asyncio.gather(
            *(_resolve_group(album_tracks) for _album_id, album_tracks in grouped)
        )

        # entries that resolved to a local file get linked so the compat shims can
        # stream them (issue #181 - imported playlists showed empty in clients)
        file_links: dict[str, str] = {}

        for (_album_id, album_tracks), (
            jf_by_num,
            local_by_num,
            nd_by_num,
            plex_by_num,
        ) in zip(grouped, resolved_maps):
            for t in album_tracks:
                sources = set()
                if t.source_type:
                    sources.add(t.source_type)

                disc_key = (t.disc_number or 1, t.track_number)
                jf_track = jf_by_num.get(disc_key)
                if jf_track and _fuzzy_name_match(t.track_name, jf_track[0]):
                    sources.add("jellyfin")

                local_track = local_by_num.get(disc_key)
                if local_track and _fuzzy_name_match(t.track_name, local_track[0]):
                    sources.add("local")
                    if not t.library_file_id and local_track[1]:
                        file_links[t.id] = local_track[1]

                nd_track = nd_by_num.get(disc_key)
                if nd_track and _fuzzy_name_match(t.track_name, nd_track[0]):
                    sources.add("navidrome")

                plex_track = plex_by_num.get(disc_key)
                if plex_track and _fuzzy_name_match(t.track_name, plex_track[0]):
                    sources.add("plex")

                result[t.id] = sorted(sources)

        for t in no_album_tracks:
            result[t.id] = [t.source_type] if t.source_type else []

        persist_updates: dict[str, list[str]] = {}
        for t in tracks:
            resolved = result.get(t.id)
            if not resolved:
                continue
            existing = set(t.available_sources) if t.available_sources else set()
            if set(resolved) >= existing and set(resolved) != existing:
                persist_updates[t.id] = resolved
        if persist_updates:
            await self._repo.batch_update_available_sources(playlist_id, persist_updates)
        if file_links:
            await self._repo.batch_link_library_files(playlist_id, file_links)

        return result

    async def _resolve_album_sources(
        self,
        album_id: str,
        jf_service: object,
        local_service: object,
        nd_service: object = None,
        plex_service: object = None,
        album_name: str = "",
        artist_name: str = "",
    ) -> tuple[dict[tuple[int, int], tuple[str, str]], dict[tuple[int, int], tuple[str, str]], dict[tuple[int, int], tuple[str, str]], dict[tuple[int, int], tuple[str, str, str]]]:
        cache_key = f"{SOURCE_RESOLUTION_PREFIX}:{album_id}"
        if self._cache:
            cached = await self._cache.get(cache_key)
            if cached is not None:
                if len(cached) == 2:
                    return (_normalize_source_map(cached[0]), _normalize_source_map(cached[1]), {}, {})
                if len(cached) == 3:
                    return (
                        _normalize_source_map(cached[0]),
                        _normalize_source_map(cached[1]),
                        _normalize_source_map(cached[2]),
                        {},
                    )
                return (
                    _normalize_source_map(cached[0]),
                    _normalize_source_map(cached[1]),
                    _normalize_source_map(cached[2]),
                    _normalize_source_map(cached[3]),
                )

        jf_by_num: dict[tuple[int, int], tuple[str, str]] = {}
        local_by_num: dict[tuple[int, int], tuple[str, str]] = {}
        nd_by_num: dict[tuple[int, int], tuple[str, str]] = {}
        plex_by_num: dict[tuple[int, int], tuple[str, str, str]] = {}

        if jf_service is not None:
            try:
                match = await jf_service.match_album_by_mbid(album_id)
                if match.found:
                    for t in match.tracks:
                        key = _safe_track_number(t.track_number)
                        if key is not None:
                            jf_by_num[(getattr(t, "disc_number", 1) or 1, key)] = (t.title, t.jellyfin_id)
            except Exception:  # noqa: BLE001
                logger.debug("Jellyfin source resolution failed for album %s", album_id, exc_info=True)

        if local_service is not None:
            try:
                match = await local_service.match_album_by_mbid(album_id)
                if match.found:
                    for t in match.tracks:
                        key = _safe_track_number(t.track_number)
                        if key is not None:
                            local_by_num[(getattr(t, "disc_number", None) or 1, key)] = (t.title, str(t.track_file_id))
            except Exception:  # noqa: BLE001
                logger.debug("Local source resolution failed for album %s", album_id, exc_info=True)

        if nd_service is not None:
            try:
                match = await nd_service.get_album_match(
                    album_id=album_id, album_name=album_name, artist_name=artist_name,
                )
                if match.found:
                    for t in match.tracks:
                        key = _safe_track_number(t.track_number)
                        if key is not None:
                            nd_by_num[(getattr(t, "disc_number", 1) or 1, key)] = (t.title, t.navidrome_id)
            except Exception:  # noqa: BLE001
                logger.debug("Navidrome source resolution failed for album %s", album_id, exc_info=True)

        if plex_service is not None:
            try:
                match = await plex_service.get_album_match(
                    album_id=album_id, album_name=album_name, artist_name=artist_name,
                )
                if match.found:
                    for t in match.tracks:
                        key = _safe_track_number(t.track_number)
                        if key is not None:
                            plex_by_num[(getattr(t, "disc_number", 1) or 1, key)] = (t.title, t.part_key or t.plex_id, t.plex_id)
            except Exception:  # noqa: BLE001
                logger.debug("Plex source resolution failed for album %s", album_id, exc_info=True)

        resolved = (jf_by_num, local_by_num, nd_by_num, plex_by_num)
        if self._cache:
            await self._cache.set(cache_key, resolved, ttl_seconds=3600)
        return resolved

    async def _resolve_new_source_id(
        self,
        track: PlaylistTrackRecord,
        new_source_type: str,
        jf_service: object,
        local_service: object,
        nd_service: object = None,
        plex_service: object = None,
    ) -> tuple[str, str | None]:
        """Return (source_id, plex_rating_key_or_none)."""
        if not track.album_id or track.track_number is None:
            raise SourceResolutionError(
                f"Cannot switch source for track '{track.track_name}': missing album_id or track_number"
            )

        jf_by_num, local_by_num, nd_by_num, plex_by_num = await self._resolve_album_sources(
            track.album_id, jf_service, local_service, nd_service, plex_service,
            album_name=track.album_name or "",
            artist_name=track.artist_name or "",
        )

        disc_key = (track.disc_number or 1, track.track_number)

        if new_source_type == "jellyfin":
            match_info = jf_by_num.get(disc_key)
            if match_info and _fuzzy_name_match(track.track_name, match_info[0]):
                return (match_info[1], None)
            raise SourceResolutionError(
                f"Track '{track.track_name}' not found in Jellyfin for album {track.album_id}"
            )

        if new_source_type == "local":
            match_info = local_by_num.get(disc_key)
            if match_info and _fuzzy_name_match(track.track_name, match_info[0]):
                return (match_info[1], None)
            raise SourceResolutionError(
                f"Track '{track.track_name}' not found in local files for album {track.album_id}"
            )

        if new_source_type == "navidrome":
            match_info = nd_by_num.get(disc_key)
            if match_info and _fuzzy_name_match(track.track_name, match_info[0]):
                return (match_info[1], None)
            raise SourceResolutionError(
                f"Track '{track.track_name}' not found in Navidrome for album {track.album_id}"
            )

        if new_source_type == "plex":
            match_info = plex_by_num.get(disc_key)
            if match_info and _fuzzy_name_match(track.track_name, match_info[0]):
                return (match_info[1], match_info[2])
            raise SourceResolutionError(
                f"Track '{track.track_name}' not found in Plex for album {track.album_id}"
            )

        raise SourceResolutionError(f"Unsupported source type for resolution: {new_source_type}")


    async def upload_cover(
        self, playlist_id: str, requesting: UserRecord, data: bytes, content_type: str,
    ) -> str:
        await self._load_owned_or_raise(playlist_id, requesting)
        self._validate_cover_id(playlist_id)

        if content_type not in ALLOWED_IMAGE_TYPES:
            raise InvalidPlaylistDataError(
                f"Invalid image type. Allowed: {', '.join(ALLOWED_IMAGE_TYPES)}"
            )
        if len(data) > MAX_COVER_SIZE:
            raise InvalidPlaylistDataError("Image too large. Maximum size is 2 MB")

        ext = _MIME_TO_EXT.get(content_type, ".jpg")
        file_path = self._cover_dir / f"{playlist_id}{ext}"

        def _write_cover() -> None:
            self._cover_dir.mkdir(parents=True, exist_ok=True)
            for old in self._cover_dir.glob(f"{playlist_id}.*"):
                try:
                    old.unlink()
                except OSError:
                    pass
            file_path.write_bytes(data)

        await asyncio.to_thread(_write_cover)

        cover_path = str(file_path)
        await self._repo.update_playlist(
            playlist_id, cover_image_path=cover_path,
        )

        cover_url = f"/api/v1/playlists/{playlist_id}/cover"
        return cover_url

    async def get_cover_path(self, playlist_id: str, requesting: UserRecord) -> Optional[Path]:
        playlist = await self.get_playlist(playlist_id)
        # Owner or public can fetch the cover; a private playlist's cover is invisible
        # to everyone else (including admins -> redaction shows no covers, D4) -> 404.
        if playlist.user_id != requesting.id and not playlist.is_public:
            raise PlaylistNotFoundError(f"Playlist {playlist_id} not found")
        if not playlist.cover_image_path:
            return None
        path = Path(playlist.cover_image_path)
        exists = await asyncio.to_thread(path.exists)
        if exists:
            return path
        return None

    async def remove_cover(self, playlist_id: str, requesting: UserRecord) -> None:
        playlist = await self._load_owned_or_raise(playlist_id, requesting)
        if playlist.cover_image_path:
            cover_path = Path(playlist.cover_image_path)
            try:
                await asyncio.to_thread(cover_path.unlink, True)
            except OSError:
                pass
        await self._repo.update_playlist(
            playlist_id, cover_image_path=None,
        )


    @staticmethod
    def _validate_cover_id(playlist_id: str) -> None:
        if not _SAFE_ID_RE.match(playlist_id):
            raise InvalidPlaylistDataError("Invalid playlist ID for cover path")

    def _delete_cover_file(self, playlist_id: str) -> None:
        if not _SAFE_ID_RE.match(playlist_id):
            return
        for f in self._cover_dir.glob(f"{playlist_id}.*"):
            try:
                f.unlink()
            except OSError:
                pass
