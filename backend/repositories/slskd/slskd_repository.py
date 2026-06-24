"""``SlskdRepository`` - the only v1 ``DownloadClientProtocol`` implementation.

Owns the search and enqueue semaphores (both 1; slskd permits only one
concurrent search and one concurrent enqueue, C3) and translates slskd JSON
shapes to/from the protocol types. slskd has NO batch id: a task is correlated
to its transfers by ``TaskRef(username, filenames)`` (C2).

Does NOT use ``from __future__ import annotations`` so its method signatures
stay structurally identical to the protocol for the conformance contract test.
"""

import asyncio
import logging
import re
from pathlib import Path

from models.common import ServiceStatus
from repositories.protocols.download_client import (
    DownloadFileRef,
    DownloadSearchResult,
    DownloadTaskStatus,
    TaskRef,
)

from .slskd_client import SlskdClient
from .slskd_models import SlskdEnqueueResponse, SlskdTransfer, SlskdUserSearchResponse

logger = logging.getLogger(__name__)

_DISC_DIR = re.compile(r"\b(?:Disc|CD)\s*\d+\b", re.IGNORECASE)
_LOSSLESS_EXT = {"flac", "alac", "wav", "ape", "wv"}


class SlskdRepository:
    # slskd fills GET /searches/{id}/responses only after the search completes, which
    # lands after the searchTimeout window (observed ~12s later on 0.25.1). Poll past
    # `timeout` by this grace or every search returns 0 candidates.
    _COMPLETION_GRACE_SECONDS = 30.0

    def __init__(
        self,
        client: SlskdClient,
        url: str,
        api_key: str,
        downloads_mount: Path,
        concurrent_searches: int = 1,
        concurrent_enqueues: int = 1,
    ):
        self._client = client
        self._url = url
        self._api_key = api_key
        self._downloads_mount = Path(downloads_mount)
        self._search_semaphore = asyncio.Semaphore(concurrent_searches)
        self._enqueue_semaphore = asyncio.Semaphore(concurrent_enqueues)

    @property
    def client_name(self) -> str:
        return "slskd"

    def is_configured(self) -> bool:
        return bool(self._url and self._api_key)

    async def health_check(self) -> ServiceStatus:
        try:
            info = await self._client.health_check()
        except Exception as exc:  # noqa: BLE001 - health check never raises
            return ServiceStatus(status="error", message=str(exc))
        version_block = info.get("version") if isinstance(info, dict) else None
        version = None
        if isinstance(version_block, dict):
            version = version_block.get("current") or version_block.get("currentVersion")
        return ServiceStatus(
            status="ok",
            version=version,
            message=f"slskd {version}" if version else "slskd",
        )

    async def search_album(
        self,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[DownloadSearchResult]:
        # escalating query breadth: a specific query sometimes returns nothing on
        # Soulseek when a broader one returns thousands (verified live). Fall back to
        # broader queries on empty; the preflight scorer narrows back down by title.
        for query in self._album_query_ladder(artist_name, album_title, year):
            results = await self._run_search(query, timeout)
            if results:
                return results
        return []

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[DownloadSearchResult]:
        # like search_album but every rung keeps the track title so the TrackMatcher
        # can pick the right recording
        for query in self._track_query_ladder(artist_name, track_title, album_title):
            results = await self._run_search(query, timeout)
            if results:
                return results
        return []

    async def enqueue(self, files: list[DownloadFileRef]) -> TaskRef:
        """Enqueue files for one peer. Correlation key is (username, filenames)
        since slskd returns no batch GUID. Serialized via Semaphore(1); the client
        retries the 429 'only one concurrent operation' with backoff."""
        if not files:
            raise ValueError("enqueue requires at least one file")
        username = files[0].username
        requested = [f.filename for f in files]
        async with self._enqueue_semaphore:
            payload = [{"filename": f.filename, "size": f.size} for f in files]
            result = await self._client.enqueue(username, payload)
        if result.failed:
            logger.warning("slskd rejected %d/%d files for %s", len(result.failed), len(files), username)
        # correlation key must reflect what slskd accepted, not the input set, or
        # get_status/cancel poll forever on transfers never created for rejected files
        return TaskRef(username=username, filenames=self._accepted_filenames(result, requested))

    async def get_status(self, task_ref: TaskRef) -> DownloadTaskStatus:
        transfers = await self._client.get_downloads(task_ref.username)
        wanted = set(task_ref.filenames)
        matched = [t for t in transfers if t.filename in wanted]
        return self._aggregate_status(task_ref, matched)

    async def cancel(self, task_ref: TaskRef) -> bool:
        transfers = await self._client.get_downloads(task_ref.username)
        wanted = set(task_ref.filenames)
        ok = True
        for transfer in transfers:
            if transfer.filename in wanted:
                ok = await self._client.cancel_transfer(task_ref.username, transfer.id) and ok
        return ok

    async def get_file_path(self, username: str, remote_filename: str) -> Path | None:
        """Resolve a finished transfer to its on-disk path inside the mounted
        slskd downloads dir, or ``None`` if it can't be located there.

        slskd writes completed files as ``{downloads}/{last remote folder}/
        {filename}`` - it preserves only the LEAF remote directory, NOT the full
        remote path (verified on 0.25.1). Resolution is existence-aware with
        fallbacks; the remote filename is untrusted, so every candidate is confined
        to the mount."""
        parts = [p for p in re.split(r"[\\/]", remote_filename) if p and p not in (".", "..")]
        if not parts:
            return None
        mount = self._downloads_mount.resolve()
        basename = parts[-1]

        def _within_mount(candidate: Path) -> Path | None:
            resolved = candidate.resolve()
            if not resolved.is_relative_to(mount):
                logger.warning("slskd path escapes the downloads mount: %r", remote_filename)
                return None
            return resolved

        # 1. slskd's actual layout: {mount}/{leaf remote folder}/{filename}.
        if len(parts) >= 2:
            leaf = _within_mount(mount / parts[-2] / basename)
            if leaf is not None and leaf.exists():
                return leaf
        # 2. Flat layout: {mount}/{filename}.
        flat = _within_mount(mount / basename)
        if flat is not None and flat.exists():
            return flat
        # 3. slskd may have sanitised the folder name - scan one level down for it.
        try:
            for child in sorted(mount.iterdir()):
                if child.is_dir():
                    cand = _within_mount(child / basename)
                    if cand is not None and cand.exists():
                        return cand
        except OSError as exc:
            logger.warning("Could not scan downloads mount %s: %s", mount, exc)
        return None

    async def _run_search(self, query: str, timeout: float) -> list[DownloadSearchResult]:
        async with self._search_semaphore:
            search = await self._client.start_search(query, timeout_seconds=timeout)
            loop = asyncio.get_running_loop()
            # poll past the search window (see _COMPLETION_GRACE_SECONDS note)
            deadline = loop.time() + timeout + self._COMPLETION_GRACE_SECONDS
            while loop.time() < deadline:
                state = await self._client.get_search_state(search.id)
                if state.is_complete:
                    responses = await self._client.get_search_responses(search.id)
                    return self._parse_search_responses(responses)
                await asyncio.sleep(0.5)
            logger.warning(
                "slskd search %s did not complete within %.0fs",
                search.id,
                timeout + self._COMPLETION_GRACE_SECONDS,
            )
            return []

    @staticmethod
    def _build_album_query(artist: str, album: str, year: int | None) -> str:
        parts = [artist, album]
        if year:
            parts.append(str(year))
        return SlskdRepository._sanitize_query(" - ".join(parts))

    @staticmethod
    def _build_track_query(artist: str, track: str, album: str | None) -> str:
        parts = [artist, track]
        if album:
            parts.append(album)
        return SlskdRepository._sanitize_query(" - ".join(parts))

    @staticmethod
    def _album_query_ladder(artist: str, album: str, year: int | None) -> list[str]:
        """Most-specific-first album queries: artist+album+year -> artist+album
        -> artist. The broadest rung relies on the preflight scorer to narrow
        the larger result set back down by title."""
        return SlskdRepository._dedupe_queries(
            [
                SlskdRepository._build_album_query(artist, album, year),
                SlskdRepository._build_album_query(artist, album, None),
                SlskdRepository._sanitize_query(artist),
            ]
        )

    @staticmethod
    def _track_query_ladder(artist: str, track: str, album: str | None) -> list[str]:
        """Most-specific-first track queries: artist+track+album -> artist+track.
        Keeps the track title at every rung so the TrackMatcher can match."""
        return SlskdRepository._dedupe_queries(
            [
                SlskdRepository._build_track_query(artist, track, album),
                SlskdRepository._build_track_query(artist, track, None),
            ]
        )

    @staticmethod
    def _dedupe_queries(queries: list[str]) -> list[str]:
        seen: set[str] = set()
        out: list[str] = []
        for q in queries:
            if q and q not in seen:
                seen.add(q)
                out.append(q)
        return out

    @staticmethod
    def _sanitize_query(query: str) -> str:
        """Strip Soulseek operators (space-surrounded hyphens, parentheses)
        that confuse the search, while preserving hyphens inside names like
        ``AC-DC`` / ``Jay-Z``."""
        query = re.sub(r"\s-\s", " ", query)
        for op in ("(", ")"):
            query = query.replace(op, " ")
        return " ".join(query.split())

    @staticmethod
    def _parse_search_responses(
        responses: list[SlskdUserSearchResponse],
    ) -> list[DownloadSearchResult]:
        results: list[DownloadSearchResult] = []
        for resp in responses:
            for file in resp.files:
                parts = re.split(r"[\\/]", file.filename)
                parent = parts[-2] if len(parts) >= 2 else ""
                # Walk up past disc-pattern directories so multi-disc albums
                # group by the album-level folder.
                if parent and _DISC_DIR.search(parent):
                    parent = parts[-3] if len(parts) >= 3 else parent
                results.append(
                    DownloadSearchResult(
                        username=resp.username,
                        filename=file.filename,
                        parent_directory=parent,
                        size=file.size,
                        extension=SlskdRepository._extension_from_filename(file.filename),
                        bitrate=file.bit_rate,
                        bit_depth=file.bit_depth,
                        sample_rate=file.sample_rate,
                        duration=file.length,
                        has_free_slot=resp.has_free_upload_slot,
                        upload_speed=resp.upload_speed or 0,
                    )
                )
        return results

    @staticmethod
    def _extension_from_filename(filename: str) -> str:
        """Lowercase extension parsed from the filename (slskd's ``extension``
        field is unreliable, C6a)."""
        base = re.split(r"[\\/]", filename)[-1]
        stem, dot, ext = base.rpartition(".")
        return ext.lower() if dot and stem else ""

    @staticmethod
    def _state_flags(state: str) -> set[str]:
        return {flag.strip().lower() for flag in state.split(",") if flag.strip()}

    @staticmethod
    def _accepted_filenames(result: SlskdEnqueueResponse, requested: list[str]) -> list[str]:
        """Filenames slskd actually accepted. Enqueued/Failed entries are
        untyped: extract filenames when present, else requested-minus-failed,
        else the full requested set."""

        def names(entries: list) -> list[str]:
            out: list[str] = []
            for entry in entries:
                if isinstance(entry, dict) and entry.get("filename"):
                    out.append(entry["filename"])
                elif isinstance(entry, str):
                    out.append(entry)
            return out

        enqueued = names(result.enqueued)
        if enqueued:
            return enqueued
        failed = set(names(result.failed))
        return [f for f in requested if f not in failed] if failed else requested

    def _aggregate_status(
        self, task_ref: TaskRef, transfers: list[SlskdTransfer]
    ) -> DownloadTaskStatus:
        files_total = len(task_ref.filenames)
        bytes_total = sum(t.size for t in transfers)
        bytes_downloaded = sum(t.bytes_transferred for t in transfers)
        completed = 0
        failed = 0
        succeeded_filenames: list[str] = []
        has_active_transfer = False
        for transfer in transfers:
            flags = self._state_flags(transfer.state)
            if "succeeded" in flags:
                completed += 1
                succeeded_filenames.append(transfer.filename)
            elif flags & {"errored", "cancelled", "failed", "rejected", "timedout"}:
                failed += 1
            elif flags & {"inprogress", "initializing"}:
                has_active_transfer = True

        progress = (bytes_downloaded / bytes_total * 100.0) if bytes_total else 0.0

        # Terminal only once every enqueued file has a terminal matched transfer,
        # so a not-yet-materialised record can't trigger a premature terminal state.
        all_terminal = (
            bool(transfers)
            and (completed + failed) == len(transfers)
            and (completed + failed) >= files_total > 0
        )
        if all_terminal and failed == 0 and completed == files_total:
            status = "completed"
        elif all_terminal and completed > 0:
            status = "partial"  # some succeeded, some failed
        elif all_terminal:
            status = "failed"  # all terminal, none succeeded
        elif completed or bytes_downloaded:
            status = "downloading"
        else:
            status = "queued"

        return DownloadTaskStatus(
            task_id="",
            status=status,
            files_total=files_total,
            files_completed=completed,
            files_failed=failed,
            bytes_total=bytes_total,
            bytes_downloaded=bytes_downloaded,
            progress_percent=progress,
            succeeded_filenames=succeeded_filenames,
            has_active_transfer=has_active_transfer,
        )
