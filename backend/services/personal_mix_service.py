"""Per-user "Your Weekly Mix" playlist.

Seeded from ListenBrainz's own recommendation playlists (weekly-jams +
weekly-exploration, the same source ``weekly_exploration_service.py`` reads)
and topped up with a similar-artist expansion (reusing
``queue_strategies.build_similar_artist_pools``, the same building block
``radio_service.py`` uses). Missing albums are optionally auto-requested
through the native download engine, mirroring the auto-download dispatch in
``new_release_service.py``.
"""

import asyncio
import logging
from datetime import datetime, timedelta, timezone
from typing import Any

import msgspec

from core.exceptions import ConfigurationError
from infrastructure.cover_urls import release_group_cover_url
from repositories.listenbrainz_models import ListenBrainzArtist
from services.discover.mbid_resolution_service import MbidResolutionService
from services.discover.queue_strategies import (
    build_similar_artist_pools,
    round_robin_dedup_select,
)
from services.native.download_service import ALREADY_IN_LIBRARY

logger = logging.getLogger(__name__)

_PLAYLIST_NAME = "Your Weekly Mix"
_SOURCE_REF_PREFIX = "personal-mix:"
_REFRESH_MIN_INTERVAL = timedelta(days=6)  # LB refreshes weekly-jams/-exploration weekly
_RECOMMENDATION_PATCHES = ("weekly-jams", "weekly-exploration")
_MAX_SEED_ARTISTS = 8
_SIMILAR_LIMIT = 10
_ALBUMS_PER_SEED = 2
_TRACK_CAP = 100


class PersonalMixResult(msgspec.Struct, frozen=True):
    user_id: str
    skipped: bool = False
    reason: str = ""
    playlist_id: str | None = None
    track_count: int = 0
    requested_albums: int = 0


class PersonalMixSummary(msgspec.Struct, frozen=True):
    users_considered: int = 0
    built: int = 0
    skipped: int = 0
    errors: int = 0


class _MixTrack(msgspec.Struct, frozen=True):
    track_name: str
    artist_name: str
    album_name: str
    release_group_mbid: str
    artist_mbid: str | None
    recording_mbid: str | None
    in_library: bool
    library_file_id: str | None = None
    track_number: int | None = None
    disc_number: int = 1


class PersonalMixService:
    def __init__(
        self,
        client_factory,
        mb_repo,
        library_repo,
        playlist_service,
        download_service,
        listening_prefs_store,
        connections_store,
        auth_store,
    ) -> None:
        self._client_factory = client_factory
        self._mb_repo = mb_repo
        self._library_repo = library_repo
        self._playlists = playlist_service
        self._downloads = download_service
        self._prefs = listening_prefs_store
        self._connections_store = connections_store
        self._auth_store = auth_store

    async def build_for_user(self, user_id: str, *, force: bool = False) -> PersonalMixResult:
        lb_repo = await self._client_factory.resolve_listenbrainz(user_id)
        if lb_repo is None:
            return PersonalMixResult(user_id=user_id, skipped=True, reason="listenbrainz_not_linked")

        username = await self._client_factory.resolve_listenbrainz_username(user_id)
        source_ref = f"{_SOURCE_REF_PREFIX}{user_id}"
        existing = await self._playlists.get_by_source_ref(source_ref, user_id)
        if not force and existing is not None and self._is_fresh(existing.updated_at):
            return PersonalMixResult(
                user_id=user_id, skipped=True, reason="fresh", playlist_id=existing.id,
            )

        owned = {
            m.lower() for m in await self._library_repo.get_library_mbids(include_release_ids=False)
        }

        lb_tracks = await self._from_recommendation_playlists(lb_repo, username, owned)
        seed_mbids = self._pick_seed_artists(lb_tracks)
        expansion_tracks: list[_MixTrack] = []
        if seed_mbids and len(lb_tracks) < _TRACK_CAP:
            expansion_tracks = await self._from_similar_artists(
                lb_repo, seed_mbids, owned, needed=_TRACK_CAP - len(lb_tracks),
            )

        mix = (lb_tracks + expansion_tracks)[:_TRACK_CAP]
        if not mix:
            return PersonalMixResult(
                user_id=user_id, skipped=True, reason="no_tracks",
                playlist_id=existing.id if existing else None,
            )
        mix = await self._match_library_files(mix)

        owner = await self._auth_store.get_user_by_id(user_id)
        if owner is None:
            return PersonalMixResult(user_id=user_id, skipped=True, reason="user_not_found")

        playlist_id = await self._upsert_playlist(existing, source_ref, owner, mix)

        requested = 0
        prefs = await self._prefs.get(user_id)
        if prefs.auto_request_personal_mix:
            requested = await self._auto_request_missing(user_id, mix)

        return PersonalMixResult(
            user_id=user_id, playlist_id=playlist_id, track_count=len(mix), requested_albums=requested,
        )

    async def run_for_all_users(self) -> PersonalMixSummary:
        user_ids = await self._connections_store.list_user_ids_for_service("listenbrainz")
        built = skipped = errors = 0
        for user_id in user_ids:
            try:
                result = await self.build_for_user(user_id)
            except Exception:  # noqa: BLE001 - one user must never kill the run
                logger.error("Personal mix build failed for user %s", user_id, exc_info=True)
                errors += 1
                continue
            if result.skipped:
                skipped += 1
            else:
                built += 1
        summary = PersonalMixSummary(
            users_considered=len(user_ids), built=built, skipped=skipped, errors=errors,
        )
        logger.info("Personal mix refresh complete: %s", summary)
        return summary

    @staticmethod
    def _is_fresh(updated_at: str) -> bool:
        if not updated_at:
            return False
        try:
            ts = datetime.fromisoformat(updated_at)
        except ValueError:
            return False
        if ts.tzinfo is None:
            ts = ts.replace(tzinfo=timezone.utc)
        return datetime.now(timezone.utc) - ts < _REFRESH_MIN_INTERVAL

    async def _from_recommendation_playlists(
        self, lb_repo, username: str | None, owned: set[str],
    ) -> list[_MixTrack]:
        try:
            playlists = await lb_repo.get_recommendation_playlists(username)
        except Exception:  # noqa: BLE001
            logger.warning("Personal mix: failed to fetch LB recommendation playlists", exc_info=True)
            return []
        if not playlists:
            return []

        wanted = []
        for patch in _RECOMMENDATION_PATCHES:
            match = next(
                (p for p in playlists if p.get("source_patch") == patch and p.get("playlist_id")),
                None,
            )
            if match:
                wanted.append(match)

        tracks: list[_MixTrack] = []
        seen_recordings: set[str] = set()
        for entry in wanted:
            try:
                playlist = await lb_repo.get_playlist_tracks(entry["playlist_id"])
            except Exception:  # noqa: BLE001
                logger.debug(
                    "Personal mix: failed to fetch playlist %s", entry.get("playlist_id"), exc_info=True,
                )
                continue
            if not playlist or not playlist.tracks:
                continue

            unique_release_ids = list({
                t.caa_release_mbid for t in playlist.tracks if t.caa_release_mbid
            })
            rg_results = await asyncio.gather(
                *(self._mb_repo.get_release_group_id_from_release(rid) for rid in unique_release_ids),
                return_exceptions=True,
            )
            release_to_rg = {
                rid: rg for rid, rg in zip(unique_release_ids, rg_results)
                if isinstance(rg, str) and rg
            }

            for t in playlist.tracks:
                if not t.caa_release_mbid:
                    continue
                rg_mbid = release_to_rg.get(t.caa_release_mbid)
                if not rg_mbid:
                    continue
                rg_mbid = rg_mbid.lower()
                if t.recording_mbid:
                    if t.recording_mbid in seen_recordings:
                        continue
                    seen_recordings.add(t.recording_mbid)
                tracks.append(_MixTrack(
                    track_name=t.title,
                    artist_name=t.creator,
                    album_name=t.album,
                    release_group_mbid=rg_mbid,
                    artist_mbid=t.artist_mbids[0] if t.artist_mbids else None,
                    recording_mbid=t.recording_mbid,
                    in_library=rg_mbid in owned,
                ))
        return tracks[:_TRACK_CAP]

    async def _match_library_files(self, mix: list[_MixTrack]) -> list[_MixTrack]:
        rg_mbids = {t.release_group_mbid for t in mix if t.in_library}
        if not rg_mbids:
            return mix

        rg_mbids = list(rg_mbids)
        results = await asyncio.gather(
            *(self._library_repo.get_tracks(rg) for rg in rg_mbids),
            return_exceptions=True,
        )
        by_rg_recording: dict[tuple[str, str], Any] = {}
        for rg, album_tracks in zip(rg_mbids, results):
            if isinstance(album_tracks, BaseException):
                logger.debug("Personal mix: library lookup failed for %s", rg, exc_info=album_tracks)
                continue
            for lt in album_tracks:
                if lt.recording_mbid:
                    by_rg_recording[(rg, lt.recording_mbid)] = lt

        matched: list[_MixTrack] = []
        for t in mix:
            lt = by_rg_recording.get((t.release_group_mbid, t.recording_mbid)) if t.recording_mbid else None
            if lt is None:
                matched.append(t)
                continue
            matched.append(msgspec.structs.replace(
                t, library_file_id=lt.id, track_number=lt.track_number, disc_number=lt.disc_number,
            ))
        return matched

    @staticmethod
    def _pick_seed_artists(tracks: list[_MixTrack]) -> list[str]:
        seen: list[str] = []
        for t in tracks:
            if t.artist_mbid and t.artist_mbid not in seen:
                seen.append(t.artist_mbid)
            if len(seen) >= _MAX_SEED_ARTISTS:
                break
        return seen

    async def _from_similar_artists(
        self, lb_repo, seed_mbids: list[str], owned: set[str], needed: int,
    ) -> list[_MixTrack]:
        mbid_svc = MbidResolutionService(
            musicbrainz_repo=self._mb_repo, library_repo=self._library_repo, listenbrainz_repo=lb_repo,
        )
        seeds = [
            ListenBrainzArtist(artist_name=mbid, artist_mbids=[mbid], listen_count=0)
            for mbid in seed_mbids
        ]
        try:
            pools = await build_similar_artist_pools(
                seeds,
                excluded_mbids=owned,
                similar_limit=_SIMILAR_LIMIT,
                albums_per=_ALBUMS_PER_SEED,
                lb_repo=lb_repo,
                mbid_svc=mbid_svc,
            )
        except Exception:  # noqa: BLE001
            logger.warning("Personal mix: similar-artist expansion failed", exc_info=True)
            return []

        # extra headroom: some candidate artists won't have a top-recording hit below
        candidates = round_robin_dedup_select(pools, needed * 2)

        tracks: list[_MixTrack] = []
        top_track_cache: dict[str, Any] = {}
        for item in candidates:
            if len(tracks) >= needed:
                break
            artist_mbid = item.artist_mbid
            if artist_mbid not in top_track_cache:
                try:
                    recordings = await lb_repo.get_artist_top_recordings(artist_mbid, count=1)
                except Exception:  # noqa: BLE001
                    recordings = []
                top_track_cache[artist_mbid] = recordings[0] if recordings else None
            recording = top_track_cache[artist_mbid]
            if recording is None:
                continue
            tracks.append(_MixTrack(
                track_name=recording.track_name,
                artist_name=item.artist_name,
                album_name=item.album_name,
                release_group_mbid=item.release_group_mbid,
                artist_mbid=artist_mbid,
                recording_mbid=recording.recording_mbid,
                in_library=False,  # excluded_mbids already filtered owned albums out of the pool
            ))
        return tracks

    async def _upsert_playlist(self, existing, source_ref: str, owner, mix: list[_MixTrack]) -> str:
        if existing is None:
            playlist = await self._playlists.create_playlist(
                _PLAYLIST_NAME, source_ref=source_ref, user_id=owner.id,
            )
            playlist_id = playlist.id
        else:
            playlist_id = existing.id
            current_tracks = await self._playlists.get_tracks(playlist_id)
            if current_tracks:
                await self._playlists.remove_tracks(
                    playlist_id, owner, [t.id for t in current_tracks],
                )

        track_dicts = [
            {
                "track_name": t.track_name,
                "artist_name": t.artist_name,
                "album_name": t.album_name,
                "album_id": t.release_group_mbid,
                "artist_id": t.artist_mbid,
                "track_source_id": t.library_file_id or t.recording_mbid,
                "cover_url": release_group_cover_url(t.release_group_mbid, size=250),
                "source_type": "local" if t.library_file_id else "",
                "track_number": t.track_number,
                "disc_number": t.disc_number,
                "library_file_id": t.library_file_id,
            }
            for t in mix
        ]
        await self._playlists.add_tracks(playlist_id, owner, track_dicts)
        return playlist_id

    async def _auto_request_missing(self, user_id: str, mix: list[_MixTrack]) -> int:
        requested = 0
        seen_rgs: set[str] = set()
        for t in mix:
            if t.in_library or t.release_group_mbid in seen_rgs:
                continue
            seen_rgs.add(t.release_group_mbid)
            try:
                task_id = await self._downloads.request_album(
                    user_id=user_id,
                    release_group_mbid=t.release_group_mbid,
                    artist_name=t.artist_name,
                    album_title=t.album_name,
                    artist_mbid=t.artist_mbid,
                    origin="user",
                )
            except ConfigurationError:
                logger.info(
                    "Personal mix: download client disabled; not auto-requesting for %s", user_id,
                )
                break  # disabled globally - further attempts this run would fail identically
            except Exception:  # noqa: BLE001 - one failed request must not abort the rest
                logger.error(
                    "Personal mix: failed to auto-request %s", t.release_group_mbid, exc_info=True,
                )
                continue
            if task_id and task_id != ALREADY_IN_LIBRARY:
                requested += 1
        return requested
