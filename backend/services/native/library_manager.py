"""LibraryManager - the native library data layer.

Aggregation-on-read: albums are a ``GROUP BY release_group_mbid`` over the file
table; there is no materialised album table or nightly reconcile.

Inherits safe-default shims from ``LibraryStub`` for the surface not-yet-migrated
services still call, overriding only the methods it implements for real; the
inherited shims are temporary bridges that shrink as consumers migrate.
"""

import asyncio
import hashlib
import logging
import os
import time
from asyncio import Lock
from pathlib import Path

from infrastructure.msgspec_fastapi import AppStruct
from models.audio import AudioInfo, AudioTag
from services.native.stubs import LibraryStub

logger = logging.getLogger(__name__)


def _synth_artist_mbid(name: str | None) -> str:
    """Synthesize a stable 32-hex (uuid-shaped, dashless) artist id from a name
    when MusicBrainz gave us none (Q14). Empty/None falls back to a single
    "Unknown Artist" bucket rather than sha1("")."""
    label = (name or "").strip() or "Unknown Artist"
    return hashlib.sha1(label.lower().encode("utf-8")).hexdigest()[:32]


def _real_mbid(value: str | None) -> str | None:
    """The value if it is a real (dashed) MusicBrainz id, else None - synthetic
    Q14 ids are dashless and must never leave the library layer as if real."""
    return value if value and "-" in value else None


def _tag_is_compilation(tag: AudioTag) -> bool:
    return bool(tag.compilation or tag.album_artist == "Various Artists")


_AUDIO_SUFFIXES = {".flac", ".mp3", ".m4a", ".m4b", ".mp4", ".ogg", ".oga", ".opus", ".wav"}
# files imported within this window are protected from a reconcile race with the
# orchestrator that may have just moved them
_DOWNLOAD_PROTECT_WINDOW_SECONDS = 300.0


class LibraryAlbumSummary(AppStruct):
    """One aggregated album row (a ``GROUP BY release_group_mbid`` result)."""

    release_group_mbid: str
    album_title: str
    album_artist_name: str | None = None
    track_count: int = 0
    total_size_bytes: int = 0
    quality_format: str | None = None
    year: int | None = None
    is_compilation: bool = False
    cover_url: str | None = None
    last_imported_at: float | None = None


class LibraryTrack(AppStruct):
    """One track row from ``library_files`` (ordered by disc/track)."""

    id: str = ""  # library_files UUID, the key the admin tag-editor writes to
    recording_mbid: str | None = None
    disc_number: int = 1
    track_number: int = 0
    track_title: str = ""
    artist_name: str | None = None
    file_path: str = ""
    file_format: str | None = None
    bit_rate: int | None = None
    sample_rate: int | None = None
    bit_depth: int | None = None
    duration_seconds: float | None = None
    file_size_bytes: int = 0
    # Quality-upgrade annotations (CollectionManagement Feature B): the file's tier
    # and whether it sits below the active cutoff while upgrades are on. Drives the
    # admin/trusted per-track upgrade affordance on the album page.
    current_tier: str | None = None
    below_cutoff: bool = False


class LibraryTrackListItem(AppStruct):
    """One row of the flat, cross-album Tracks view. Unlike ``LibraryTrack`` it
    carries album/artist/cover context so the player can queue a track in isolation."""

    track_file_id: str
    title: str
    album_name: str = ""
    artist_name: str = ""
    album_mbid: str | None = None
    cover_url: str | None = None
    format: str = ""
    track_number: int = 0
    disc_number: int = 1
    duration_seconds: float | None = None


class LibraryArtistSummary(AppStruct):
    artist_name: str
    artist_mbid: str | None = None
    album_count: int = 0
    track_count: int = 0
    date_added: float | None = None


class LibraryStats(AppStruct):
    total_albums: int = 0
    total_artists: int = 0
    total_tracks: int = 0
    total_size_bytes: int = 0
    format_breakdown: dict[str, int] = {}
    unmatched_count: int = 0
    last_scan_at: float | None = None
    recently_added: list[LibraryAlbumSummary] = []


class UnmatchedFile(AppStruct):
    """One manual-review-queue row (a file the scanner couldn't identify)."""

    id: int
    file_path: str
    extracted_title: str | None = None
    extracted_artist: str | None = None
    extracted_album: str | None = None
    extracted_year: int | None = None
    track_number: int | None = None
    disc_number: int | None = None
    file_format: str | None = None
    duration: float | None = None
    file_size: int | None = None
    fingerprint: str | None = None
    fingerprint_score: float | None = None
    candidate_mbids: list[str] = []
    source: str = ""
    created_at: float | None = None


class LibraryAlbumStatus(AppStruct):
    """Combined album-page payload."""

    in_library: bool
    track_count: int = 0
    tracks: list[LibraryTrack] = []


class LibraryManager(LibraryStub):
    def __init__(self, library_db) -> None:  # noqa: ANN001 - LibraryDB, avoid import cycle
        super().__init__()
        self._db = library_db
        # serialises read-modify-write so concurrent async tasks can't interleave
        # between SELECT and write. lock ordering: async lock → to_thread() →
        # PersistenceBase threading.Lock
        self._library_write_lock = Lock()

    def is_configured(self) -> bool:
        return True  # always available; data may be empty

    # is_library_empty() stays the inherited sync stub: the protocol method is sync
    # so it can't await the DB

    async def has_album(self, mbid: str) -> bool:
        return await self._db.has_album_files(mbid)

    async def album_quality_tier(self, release_group_mbid: str) -> str | None:
        """The WORST quality tier across the album's held files (an album is only as good as
        its weakest track - mirrors ``candidate_tier``), or ``None`` when the album isn't in
        the library. Drives the cutoff/upgrade gate (step 8)."""
        from services.native.quality_tiers import tier_for, tier_rank

        rows = await self._db.get_library_files_for_album(release_group_mbid)
        tiers = [tier_for(row.get("file_format") or "", row.get("bit_rate")) for row in rows]
        return min(tiers, key=tier_rank) if tiers else None

    async def list_cutoff_unmet(self, cutoff: str) -> list[dict]:
        """The upgrade worklist: albums whose worst held tier is below ``cutoff``,
        each row annotated with its ``current_tier`` key."""
        from services.native.quality_tiers import TIER_KEYS, tier_rank

        rows = await self._db.list_cutoff_unmet(tier_rank(cutoff))
        rank_to_key = tuple(reversed(TIER_KEYS))  # rank index -> tier key
        for row in rows:
            row["current_tier"] = rank_to_key[int(row.pop("worst_rank"))]
        return rows

    async def recording_quality_tier(self, recording_mbid: str) -> str | None:
        """The BEST held tier for one recording, or ``None`` when it isn't in the
        library. A per-track upgrade must beat the best copy the library already has
        (unlike ``album_quality_tier``, whose worst-track semantics measure album
        completeness, not what a single track's replacement must exceed)."""
        from services.native.quality_tiers import tier_for, tier_rank

        rows = await self._db.get_library_files_for_recording(recording_mbid)
        tiers = [tier_for(row.get("file_format") or "", row.get("bit_rate")) for row in rows]
        return max(tiers, key=tier_rank) if tiers else None

    async def has_track(self, recording_mbid: str) -> bool:
        return await self._db.has_recording(recording_mbid)

    async def has_any_files(self) -> bool:
        """Whether the native library holds any (non-deleted) files. Gates the
        Local Files tab + local play/download affordances (see HomeService)."""
        return await self._db.has_any_files()

    async def get_library_mbids(self, include_release_ids: bool = True) -> set[str]:
        """Release-group (and optionally release) MBIDs in the native library.

        Overrides the empty ``LibraryStub`` so the /library/mbids set, artist
        discography in_library flags, and the request-completion check all reflect
        native imports written to ``library_files``."""
        return await self._db.get_library_mbids(include_release_ids=include_release_ids)

    async def get_albums(
        self,
        page: int = 1,
        page_size: int = 50,
        sort: str = "recent",
        q: str | None = None,
        file_format: str | None = None,
    ) -> list[LibraryAlbumSummary]:
        offset = (max(page, 1) - 1) * max(page_size, 1)
        rows, _total = await self._db.get_albums_aggregated(
            limit=page_size, offset=offset, sort=sort, q=q, file_format=file_format
        )
        return [self._to_summary(row) for row in rows]

    async def get_albums_page(
        self,
        page: int = 1,
        page_size: int = 50,
        sort: str = "recent",
        q: str | None = None,
        file_format: str | None = None,
        decade: int | None = None,
    ) -> tuple[list[LibraryAlbumSummary], int]:
        """Paginated albums + total count (drives the browse route's items/total)."""
        offset = (max(page, 1) - 1) * max(page_size, 1)
        rows, total = await self._db.get_albums_aggregated(
            limit=page_size, offset=offset, sort=sort, q=q, file_format=file_format, decade=decade
        )
        return [self._to_summary(row) for row in rows], total

    async def get_tracks_page(
        self,
        *,
        limit: int = 48,
        offset: int = 0,
        sort: str = "recent",
        q: str | None = None,
    ) -> tuple[list[LibraryTrackListItem], int]:
        """Paginated flat track list + total count (drives the Tracks view's
        browse and Play-All/Shuffle)."""
        rows, total = await self._db.get_tracks_paginated(
            limit=limit, offset=offset, sort=sort, q=q
        )
        return [self._to_track_list_item(row) for row in rows], total

    async def get_crate_tracks(
        self, *, order: str = "random", limit: int = 8, decade: int | None = None
    ) -> list[dict]:
        """Raw crate track rows (id/title/album/artist/cover/year) for the Listening Room."""
        return await self._db.get_crate_tracks(order=order, limit=limit, decade=decade)

    async def search_tracks(self, q: str, *, limit: int = 30) -> list[dict]:
        """Raw track rows matching ``q`` (title/artist/album) for library search."""
        return await self._db.search_tracks(q, limit=limit)

    async def get_decades(self) -> list[dict]:
        """Decade buckets ``[{decade, album_count}]`` from album years, newest first."""
        return await self._db.get_decades()

    async def get_artists(
        self,
        *,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "name",
        sort_order: str = "asc",
        q: str | None = None,
        include_synthetic_mbids: bool = False,
    ) -> tuple[list[LibraryArtistSummary], int]:
        """``include_synthetic_mbids`` is for the compat layer, whose browse-by-id
        keys off the Q14 ids; the native API must not expose them - the UI would
        link a synthetic id to a MusicBrainz-backed artist page that can only 404."""
        rows, total = await self._db.get_artists_aggregated(
            limit=limit, offset=offset, sort_by=sort_by, sort_order=sort_order, q=q
        )
        artists = [
            LibraryArtistSummary(
                artist_name=str(row.get("artist_name") or ""),
                artist_mbid=(
                    row.get("artist_mbid")
                    if include_synthetic_mbids
                    else _real_mbid(row.get("artist_mbid"))
                ),
                album_count=int(row.get("album_count") or 0),
                track_count=int(row.get("track_count") or 0),
                date_added=row.get("date_added"),
            )
            for row in rows
        ]
        return artists, total

    async def get_stats(self) -> LibraryStats:
        data = await self._db.get_library_stats()
        recently_added, _ = await self.get_albums_page(page=1, page_size=10, sort="recent")
        return LibraryStats(
            total_albums=data["total_albums"],
            total_artists=data["total_artists"],
            total_tracks=data["total_tracks"],
            total_size_bytes=data["total_size_bytes"],
            format_breakdown=data["format_breakdown"],
            unmatched_count=data["unmatched_count"],
            last_scan_at=data.get("last_scan_at"),
            recently_added=recently_added,
        )

    async def get_tracks(self, release_group_mbid: str) -> list[LibraryTrack]:
        rows = await self._db.get_library_files_for_album(release_group_mbid)
        return [self._to_track(row) for row in rows]

    async def get_album_status(
        self,
        release_group_mbid: str,
        *,
        quality_cutoff: str | None = None,
        upgrade_allowed: bool = False,
    ) -> LibraryAlbumStatus:
        from services.native.quality_tiers import tier_for, tier_rank

        tracks = await self.get_tracks(release_group_mbid)
        for track in tracks:
            track.current_tier = tier_for(track.file_format or "", track.bit_rate)
            track.below_cutoff = (
                upgrade_allowed
                and quality_cutoff is not None
                and tier_rank(track.current_tier) < tier_rank(quality_cutoff)
            )
        return LibraryAlbumStatus(
            in_library=bool(tracks),
            track_count=len(tracks),
            tracks=tracks,
        )

    async def upsert_file(
        self,
        audio_path: Path,
        tag: AudioTag,
        info: AudioInfo,
        *,
        release_group_mbid: str | None,
        release_mbid: str | None = None,
        recording_mbid: str | None = None,
        confidence: float = 1.0,
        source: str = "scan",
        download_task_id: str | None = None,
        source_path: str | None = None,
        file_mtime: float | None = None,
    ) -> str:
        """Insert or update one file's row, returning the row id. ``file_mtime`` lets
        a caller that already stat()'d the file (the scanner) skip a second blocking
        syscall on the event loop. ``source_path`` records the pre-organisation path
        so a crash-resumed import can re-correlate an already-moved file by its
        original name (see ``get_imported_file``)."""
        if release_group_mbid is None and source != "manual_review":
            # matches the table CHECK: raise a clear contract error instead of a raw
            # IntegrityError. unmatched files go via queue_for_manual_review
            raise ValueError(
                "upsert_file requires release_group_mbid unless source='manual_review'"
            )
        # one album, one album-artist identity: a file whose tags carry no real
        # album-artist MBID inherits the identity its release group already resolved
        # to, instead of synthesizing a name-hash id that splits the artist in two
        album_artist_override = None
        if (
            release_group_mbid
            and _real_mbid(tag.musicbrainz_album_artist_id) is None
            and not _tag_is_compilation(tag)
        ):
            album_artist_override = await self._db.get_album_artist_for_release_group(
                release_group_mbid
            )
        row = self._build_row(
            audio_path,
            tag,
            info,
            release_group_mbid=release_group_mbid,
            release_mbid=release_mbid,
            recording_mbid=recording_mbid,
            confidence=confidence,
            source=source,
            download_task_id=download_task_id,
            source_path=source_path,
            file_mtime=file_mtime,
            album_artist_override=album_artist_override,
        )
        async with self._library_write_lock:
            file_id = await self._db.upsert_library_file(row)
            await self._upsert_artist_rows(row)
            return file_id

    async def _upsert_artist_rows(self, row: dict) -> None:
        """Ensure a library_artists row exists for the track + album artist (Q14).
        Idempotent; needed by compat browse-by-id and the lazy-MB discovery cache,
        which key off library_artists. The native scanner is the only writer of
        library_files, so it owns these rows too (00b s7 caveat)."""
        seen: set[str] = set()
        for mbid_key, name_key in (
            ("artist_mbid", "artist_name"),
            ("album_artist_mbid", "album_artist_name"),
        ):
            mbid = row.get(mbid_key)
            if not mbid or mbid in seen:
                continue
            seen.add(mbid)
            name = (row.get(name_key) or "").strip() or "Unknown Artist"
            await self._db.upsert_artist(mbid, name)

    async def soft_delete_file(self, file_path: str) -> None:
        await self._db.soft_delete_library_file(file_path)

    async def reconcile_with_filesystem(self, targets: list[Path] | None = None) -> int:
        """Soft-delete library files no longer present under ``targets``. With no
        targets it's a safe no-op rather than a mass-delete (the full-library path
        needs the configured library paths, wired in 3b/Phase 4)."""
        if not targets:
            logger.debug("reconcile_with_filesystem called with no targets - skipping")
            return 0
        # walk each root independently so one unmounted root doesn't poison the whole
        # reconcile: a root yielding zero files is treated as unavailable and its
        # tracks protected, while healthy sibling roots still reconcile
        present: set[str] = set()
        empty_roots: list[Path] = []
        for target in targets:
            found = await asyncio.to_thread(self._walk_audio_paths, [target])
            if found:
                present |= found
            else:
                empty_roots.append(target)
        if not present:
            # os.walk returns nothing (no error) for a missing/unmounted dir, so
            # reconciling against the empty set would soft-delete the whole library;
            # refuse rather than mass-delete on a transient mount failure
            logger.warning(
                "reconcile_with_filesystem found no files under %s - skipping to avoid "
                "mass soft-delete (paths missing or unmounted?)",
                [str(t) for t in targets],
            )
            return 0
        if empty_roots:
            logger.warning(
                "reconcile_with_filesystem found no files under %s while sibling roots "
                "have files - protecting those tracks from soft-delete (paths missing "
                "or unmounted?)",
                [str(t) for t in empty_roots],
            )
        async with self._library_write_lock:
            return await self._db.mark_missing_files(
                present,
                # only soft-delete within the dirs we walked: a targeted post-import
                # reconcile passes one album folder, must not flag files elsewhere
                scope_dirs=[str(t) for t in targets],
                protect_downloads_after=time.time() - _DOWNLOAD_PROTECT_WINDOW_SECONDS,
                protected_roots=[str(t) for t in empty_roots],
            )

    async def get_unmatched(self) -> list[UnmatchedFile]:
        rows = await self._db.get_unmatched_files()
        return [self._to_unmatched(row) for row in rows]

    async def get_file_row_by_id(self, file_id: str) -> dict | None:
        """Raw ``library_files`` row by id (carries source/confidence/MBIDs the
        domain ``LibraryTrack`` drops). ``None`` if no such row."""
        return await self._db.get_library_file_by_id(file_id)

    async def get_track_by_id(self, file_id: str) -> LibraryTrack | None:
        row = await self._db.get_library_file_by_id(file_id)
        return self._to_track(row) if row is not None else None

    async def get_file_rows_for_album(self, release_group_mbid: str) -> list[dict]:
        """Raw active rows for one album (rescan needs source/confidence/MBIDs)."""
        return await self._db.get_library_files_for_album(release_group_mbid)

    async def get_attributions_for_paths(self, paths: list[str]) -> dict[str, dict]:
        """Existing attributions (release-group/confidence/source) for these paths,
        keyed by path - the scanner's anchor read for protecting known-good identity."""
        return await self._db.get_attributions_for_paths(paths)

    async def delete_album_row(self, release_group_mbid: str) -> None:
        """Drop a release group's materialised ``library_albums`` row (the /basic
        in_library source). Used after a re-identify empties an album's old RG, so it
        stops reporting 'In Library' as a zero-file ghost."""
        await self._db.delete_album_by_mbid(release_group_mbid)

    async def get_file_at_position(
        self, release_group_mbid: str, disc_number: int, track_number: int
    ) -> dict | None:
        """The active library row occupying one (disc, track) slot of an album, if
        any. The import path uses it to avoid writing a second file for a track the
        library already holds (a re-pull or a different-format copy)."""
        return await self._db.get_active_file_at_position(
            release_group_mbid, disc_number, track_number
        )

    async def get_imported_file(
        self, download_task_id: str, filename: str
    ) -> dict | None:
        """The active ``library_files`` row a prior run imported for this task +
        remote file, or ``None`` (crash-idempotency).

        The import renames files per the naming template, so ``file_path`` no longer
        carries the remote name - the original is preserved in ``source_path``.
        Matching is by the remote relative path, not the basename, so two files of one
        task sharing a basename (multi-disc ``Disc 1/01.flac`` vs ``Disc 2/01.flac``)
        aren't confused. slskd uses ``\\`` or ``/`` separators, so both sides are
        normalised before comparing."""
        if not download_task_id:
            return None
        target = filename.replace("\\", "/").strip("/")
        if not target:
            return None
        rows = await self._db.get_library_files_for_task(download_task_id)
        for row in rows:
            src = (row.get("source_path") or "").replace("\\", "/")
            if src == target or src.endswith("/" + target):
                return row
        return None

    async def get_unmatched_row_by_id(self, review_id: int) -> dict | None:
        return await self._db.get_manual_review_by_id(review_id)

    async def mark_unmatched_resolved(self, review_id: int, resolution: str) -> bool:
        return await self._db.resolve_manual_review(review_id, resolution)

    async def queue_for_manual_review(
        self,
        audio_path: Path,
        tag: AudioTag,
        info: AudioInfo,
        *,
        source: str,
        fingerprint: str | None = None,
        fingerprint_score: float | None = None,
        candidates: list[str] | None = None,
    ) -> None:
        """Record a file the scanner couldn't confidently identify (Tier 4)."""
        await self._db.add_to_manual_review(
            {
                "file_path": str(audio_path),
                "extracted_title": tag.title or None,
                "extracted_artist": tag.artist or None,
                "extracted_album": tag.album or None,
                "extracted_year": tag.year,
                "track_number": tag.track_number or None,
                "disc_number": tag.disc_number or None,
                "file_format": info.file_format,
                "duration": info.duration_seconds,
                "file_size": info.file_size_bytes,
                "fingerprint": fingerprint,
                "fingerprint_score": fingerprint_score,
                "candidate_mbids": candidates or [],
                "source": source,
            }
        )

    async def get_file_index(self) -> dict[str, tuple[float, int]]:
        """Active ``file_path -> (mtime, size)`` for the scanner's incremental skip."""
        return await self._db.get_file_index()

    async def prune_review_for_imported(self) -> int:
        return await self._db.prune_manual_review_for_imported()

    async def get_release_groups_needing_artist(self) -> list[str]:
        return await self._db.get_release_groups_needing_artist()

    async def set_album_artist(
        self, release_group_mbid: str, artist_mbid: str, artist_name: str
    ) -> int:
        return await self._db.set_album_artist(release_group_mbid, artist_mbid, artist_name)

    def _build_row(
        self,
        audio_path: Path,
        tag: AudioTag,
        info: AudioInfo,
        *,
        release_group_mbid: str | None,
        release_mbid: str | None,
        recording_mbid: str | None,
        confidence: float,
        source: str,
        download_task_id: str | None,
        source_path: str | None = None,
        file_mtime: float | None = None,
        album_artist_override: tuple[str, str] | None = None,
    ) -> dict:
        is_compilation = _tag_is_compilation(tag)
        album_artist_name = (
            "Various Artists" if is_compilation else (tag.album_artist or tag.artist)
        )
        # reuse the caller's stat() when supplied; only touch the filesystem (on the
        # event loop) as a fallback for callers without the mtime.
        if file_mtime is not None:
            mtime = file_mtime
        else:
            mtime = audio_path.stat().st_mtime if audio_path.exists() else 0.0
        # synth a stable id from the name when MB gave none, so browse-by-artist
        # (groups on album_artist_mbid) and the compat artist ids always have a key
        artist_mbid = tag.musicbrainz_artist_id or _synth_artist_mbid(tag.artist)
        album_artist_mbid = tag.musicbrainz_album_artist_id or _synth_artist_mbid(
            album_artist_name
        )
        if album_artist_override is not None and not is_compilation:
            # both mbid AND name, mirroring set_album_artist: the album's files must
            # aggregate under one (id, display name) pair
            album_artist_mbid, album_artist_name = album_artist_override
        return {
            "release_group_mbid": release_group_mbid,
            "release_mbid": release_mbid,
            "recording_mbid": recording_mbid,
            "disc_number": tag.disc_number,
            "track_number": tag.track_number,
            "track_title": tag.title,
            "artist_name": tag.artist,            # per-track (compilations)
            "artist_mbid": artist_mbid,
            "album_artist_name": album_artist_name,
            "album_artist_mbid": album_artist_mbid,
            "album_title": tag.album,
            "year": tag.year,
            "genre": tag.genre,
            "channels": info.channels,
            "file_path": str(audio_path),
            "source_path": source_path,
            "file_size_bytes": info.file_size_bytes,
            "file_mtime": mtime,
            "duration_seconds": info.duration_seconds,
            "file_format": info.file_format,
            "bit_rate": info.bitrate,
            "sample_rate": info.sample_rate,
            "bit_depth": info.bit_depth,
            "source": source,
            "confidence": confidence,
            "is_compilation": 1 if is_compilation else 0,
            "tagged_at": None,
            "download_task_id": download_task_id,
        }

    @staticmethod
    def _walk_audio_paths(targets: list[Path]) -> set[str]:
        present: set[str] = set()
        for base in targets:
            for root, dirs, files in os.walk(base):
                # skip hidden dirs so reconcile walks identically to the scanner
                dirs[:] = [d for d in dirs if not d.startswith(".")]
                for name in files:
                    if Path(name).suffix.lower() in _AUDIO_SUFFIXES:
                        present.add(str(Path(root) / name))
        return present

    @staticmethod
    def _to_summary(row: dict) -> LibraryAlbumSummary:
        return LibraryAlbumSummary(
            release_group_mbid=str(row["release_group_mbid"]),
            album_title=str(row.get("album_title") or ""),
            album_artist_name=row.get("album_artist_name"),
            track_count=int(row.get("track_count") or 0),
            total_size_bytes=int(row.get("total_size_bytes") or 0),
            quality_format=row.get("file_format"),
            year=row.get("year"),
            is_compilation=bool(row.get("is_compilation")),
            cover_url=row.get("cover_url"),
            last_imported_at=row.get("last_imported_at"),
        )

    @staticmethod
    def _to_unmatched(row: dict) -> UnmatchedFile:
        return UnmatchedFile(
            id=int(row["id"]),
            file_path=str(row.get("file_path") or ""),
            extracted_title=row.get("extracted_title"),
            extracted_artist=row.get("extracted_artist"),
            extracted_album=row.get("extracted_album"),
            extracted_year=row.get("extracted_year"),
            track_number=row.get("track_number"),
            disc_number=row.get("disc_number"),
            file_format=row.get("file_format"),
            duration=row.get("duration"),
            file_size=row.get("file_size"),
            fingerprint=row.get("fingerprint"),
            fingerprint_score=row.get("fingerprint_score"),
            candidate_mbids=[str(c) for c in (row.get("candidate_mbids") or [])],
            source=str(row.get("source") or ""),
            created_at=row.get("created_at"),
        )

    @staticmethod
    def _to_track(row: dict) -> LibraryTrack:
        return LibraryTrack(
            id=str(row.get("id") or ""),
            recording_mbid=row.get("recording_mbid"),
            disc_number=int(row.get("disc_number") or 1),
            track_number=int(row.get("track_number") or 0),
            track_title=str(row.get("track_title") or ""),
            artist_name=row.get("artist_name"),
            file_path=str(row.get("file_path") or ""),
            file_format=row.get("file_format"),
            bit_rate=row.get("bit_rate"),
            sample_rate=row.get("sample_rate"),
            bit_depth=row.get("bit_depth"),
            duration_seconds=row.get("duration_seconds"),
            file_size_bytes=int(row.get("file_size_bytes") or 0),
        )

    @staticmethod
    def _to_track_list_item(row: dict) -> LibraryTrackListItem:
        return LibraryTrackListItem(
            track_file_id=str(row.get("id") or ""),
            title=str(row.get("track_title") or ""),
            album_name=str(row.get("album_title") or ""),
            artist_name=str(row.get("artist_name") or row.get("album_artist_name") or ""),
            album_mbid=row.get("release_group_mbid"),
            cover_url=row.get("cover_url"),
            format=str(row.get("file_format") or ""),
            track_number=int(row.get("track_number") or 0),
            disc_number=int(row.get("disc_number") or 1),
            duration_seconds=row.get("duration_seconds"),
        )
