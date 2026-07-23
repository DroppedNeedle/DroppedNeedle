"""``SlskdRepository`` - the only v1 ``DownloadClientProtocol`` implementation.

Owns the search and enqueue semaphores (both 1; slskd permits only one
concurrent search and one concurrent enqueue, C3) and translates slskd JSON
shapes to/from the protocol types. slskd has NO batch id: a task is correlated
to its transfers by ``TaskHandle(source="soulseek", username, filenames)`` (C2).

Implements the download side of the split protocol (D2). The search side is
re-homed onto ``SlskdIndexer`` (an ``IndexerProtocol`` adapter wrapping this
repo's ``search_album``/``search_track``), so those two methods stay here but are
no longer part of ``DownloadClientProtocol``.

Does NOT use ``from __future__ import annotations`` so its method signatures
stay structurally identical to the protocol for the conformance contract test.
"""

import asyncio
import logging
import re
from pathlib import Path

from models.common import ServiceStatus
from repositories.protocols.download_client import (
    DownloadMaterialization,
    DownloadSearchResult,
    DownloadTaskStatus,
    EnqueueRequest,
    MountDiagnosis,
    TaskHandle,
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
    # How many finished transfers diagnose_downloads_mount tries to locate under the
    # mount. Small: it's a settings-page check and a wrong mount makes each a full walk.
    _DIAGNOSIS_SAMPLE = 3

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
            version = version_block.get("current") or version_block.get(
                "currentVersion"
            )
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

    async def enqueue(self, request: EnqueueRequest) -> TaskHandle:
        """Enqueue files for one peer. Correlation key is (username, filenames)
        since slskd returns no batch GUID. Serialized via Semaphore(1); the client
        retries the 429 'only one concurrent operation' with backoff."""
        files = request.files
        if not files:
            raise ValueError("enqueue requires at least one file")
        username = files[0].username
        requested = [f.filename for f in files]
        async with self._enqueue_semaphore:
            payload = [{"filename": f.filename, "size": f.size} for f in files]
            result = await self._client.enqueue(username, payload)
        if result.failed:
            logger.warning(
                "slskd rejected %d/%d files for %s",
                len(result.failed),
                len(files),
                username,
            )
        # correlation key must reflect what slskd accepted, not the input set, or
        # get_status/cancel poll forever on transfers never created for rejected files
        return TaskHandle(
            source="soulseek",
            username=username,
            filenames=self._accepted_filenames(result, requested),
        )

    async def get_status(self, handle: TaskHandle) -> DownloadTaskStatus:
        transfers = await self._client.get_downloads(handle.username)
        wanted = set(handle.filenames)
        matched = [t for t in transfers if t.filename in wanted]
        return self._aggregate_status(handle, matched)

    async def abort(self, handle: TaskHandle) -> bool:
        return await self._remove_transfer_records(handle)

    async def inspect_materialization(
        self, handle: TaskHandle
    ) -> DownloadMaterialization:
        status = await self.get_status(handle)
        paths = await self.list_completed_files(handle)
        healthy = await asyncio.to_thread(self._downloads_mount_healthy)
        if status.status in {"completed"}:
            state = "completed"
        elif status.status in {"partial", "failed"}:
            state = "failed"
        elif status.matched_transfers:
            state = "active"
        else:
            state = "missing"
        return DownloadMaterialization(
            state=state,
            mount_root=str(self._downloads_mount),
            file_paths=[str(path) for path in paths],
            mount_healthy=healthy,
        )

    async def discard_client_artifacts(self, handle: TaskHandle) -> bool:
        return await self._remove_transfer_records(handle)

    async def _remove_transfer_records(self, handle: TaskHandle) -> bool:
        transfers = await self._client.get_downloads(handle.username)
        wanted = set(handle.filenames)
        ok = True
        for transfer in transfers:
            if transfer.filename in wanted:
                ok = (
                    await self._client.cancel_transfer(handle.username, transfer.id)
                    and ok
                )
        return ok

    def _downloads_mount_healthy(self) -> bool:
        try:
            if not self._downloads_mount.is_dir():
                return False
            next(self._downloads_mount.iterdir(), None)
            return True
        except OSError:
            return False

    async def list_completed_files(self, handle: TaskHandle) -> list[Path]:
        """slskd already knows its filenames (from the search/handle), so resolve
        each to its on-disk path via the same locator the import uses; unresolved
        files are skipped. (SABnzbd, by contrast, enumerates an unpacked folder.)"""
        paths: list[Path] = []
        for filename in handle.filenames:
            located = await self.get_file_path(handle, filename)
            if located is not None:
                paths.append(located)
        return paths

    async def get_file_path(
        self, handle: TaskHandle, remote_filename: str, size: int | None = None
    ) -> Path | None:
        """Resolve a finished transfer to its on-disk path, OFF the event loop.

        The lookup does bounded but potentially large filesystem walks; running it
        inline froze the whole loop (polling, SSE, every request - including the cancel
        the user is trying to click) whenever the mount was big or misconfigured, which
        reads as "it won't cancel and the whole app hangs"."""
        return await asyncio.to_thread(
            self._locate_file, handle.username, remote_filename, size
        )

    def _locate_file(
        self, username: str, remote_filename: str, size: int | None = None
    ) -> Path | None:
        """Resolve a finished transfer to its on-disk path inside the mounted
        slskd downloads dir, or ``None`` if it can't be located there. Sync (runs in a
        worker thread via ``get_file_path``); does only filesystem I/O, no awaits.

        slskd's on-disk layout varies by version and by how the peer organised
        their share: ``{downloads}/{leaf remote folder}/{file}`` (common),
        ``{downloads}/{username}/{file}`` or ``.../{username}/{album}/{file}``
        (peers that file by user), or a flat dump. We try the cheap direct paths
        first, then a username-scoped walk at any depth (scoped so a same-named
        track from another peer can't be grabbed), and finally an exact byte-size
        match for when slskd sanitised the on-disk filename and the basename no
        longer matches. The remote filename is untrusted, so every candidate is
        confined to the mount."""
        parts = [
            p for p in re.split(r"[\\/]", remote_filename) if p and p not in (".", "..")
        ]
        if not parts:
            return None
        mount = self._downloads_mount.resolve()
        basename = parts[-1]

        def _within_mount(candidate: Path) -> Path | None:
            resolved = candidate.resolve()
            if not resolved.is_relative_to(mount):
                logger.warning(
                    "slskd path escapes the downloads mount: %r", remote_filename
                )
                return None
            return resolved

        # 1. slskd's common layout: {mount}/{leaf remote folder}/{filename}.
        if len(parts) >= 2:
            leaf = _within_mount(mount / parts[-2] / basename)
            if leaf is not None and leaf.exists():
                return leaf
        # 2. Flat layout: {mount}/{filename}.
        flat = _within_mount(mount / basename)
        if flat is not None and flat.exists():
            return flat
        # 3. Peers that file by username: walk {mount}/{username}/ at any depth
        # (covers {username}/{file} and {username}/{album}/{file}). Scoped to the
        # peer so a same-named track from a different user can't be picked up.
        user_root = _within_mount(mount / username) if username else None
        if user_root is not None and user_root.is_dir():
            hit = self._walk_find(user_root, mount, lambda e: e.name == basename)
            if hit is not None:
                return hit
        # 4. slskd may have sanitised the folder name - scan one level down for it.
        try:
            for child in sorted(mount.iterdir()):
                if child.is_dir():
                    cand = _within_mount(child / basename)
                    if cand is not None and cand.exists():
                        return cand
        except OSError as exc:
            logger.warning("Could not scan downloads mount %s: %s", mount, exc)
        # 5. Last resort: slskd sanitised the FILENAME (illegal chars stripped), so
        # the basename no longer matches. An exact byte-size match under the peer's
        # folder recovers it - size is a strong key and the scope keeps it precise.
        if size and user_root is not None and user_root.is_dir():

            def _matches_size(entry: Path) -> bool:
                try:
                    return entry.stat().st_size == size
                except OSError:
                    return False

            hit = self._walk_find(user_root, mount, _matches_size)
            if hit is not None:
                return hit

        # 6. Whole-mount fallback for a file nested deeper than the cheap steps look,
        # under a folder that isn't the peer's username (e.g. {downloads}/{artist}/
        # {album}/{file}). Exact basename match across the mount, validated by byte size
        # when known: a name+size match is effectively the same file, so this can't grab
        # a different same-named track - an unscoped size-ONLY walk would cross peers
        # (step 5 stays peer-scoped on purpose). Reached only after every step missed.
        def _name_size_match(entry: Path) -> bool:
            if entry.name != basename:
                return False
            if not size:
                return True
            try:
                return entry.stat().st_size == size
            except OSError:
                return False

        hit = self._walk_find(mount, mount, _name_size_match)
        if hit is not None:
            return hit
        try:
            top_level = sum(1 for _ in mount.iterdir())
        except OSError:
            top_level = -1
        logger.warning(
            "slskd file not locatable on the downloads mount: %s (%s bytes); "
            "%d top-level entries under the mount - the on-disk layout may nest deeper "
            "or sanitise names beyond what get_file_path handles",
            basename,
            size if size else "unknown",
            top_level,
        )
        return None

    @staticmethod
    def _walk_find(root: Path, mount: Path, predicate) -> Path | None:
        """First file under ``root`` (bounded DFS, exact-name compare so glob
        metacharacters in filenames are harmless) for which ``predicate`` is true,
        confined to ``mount``. The entry cap is a backstop against a pathological
        tree or a symlink loop."""
        max_entries = 10000
        try:
            stack = [root]
            seen = 0
            while stack:
                for entry in stack.pop().iterdir():
                    seen += 1
                    if seen > max_entries:
                        return None
                    if entry.is_dir():
                        stack.append(entry)
                        continue
                    if not entry.is_file() or not predicate(entry):
                        continue
                    resolved = entry.resolve()
                    if resolved.is_relative_to(mount):
                        return resolved
        except OSError:
            return None
        return None

    async def diagnose_downloads_mount(self) -> MountDiagnosis:
        """Cross-check slskd's completed (not-yet-imported) downloads against the
        configured mount. slskd having finished downloads that DN can't locate under
        the mount means the path is wrong/too-broad or unreadable - the silent misconfig
        the per-download error only reveals one file at a time. Best-effort: never raises.

        The honest test is whether a sample of those finished files actually RESOLVE
        under the mount (``resolvable_downloads``); ``mount_has_files`` is a weaker
        signal that a parent-of-downloads mount (e.g. the whole library) defeats."""
        client_dir = await self._configured_downloads_dir()
        try:
            transfers = await self._client.get_all_downloads()
        except Exception:  # noqa: BLE001 - a diagnostic must never raise
            return MountDiagnosis(supported=True, client_downloads_dir=client_dir)
        completed = [t for t in transfers if "succeeded" in self._state_flags(t.state)]
        if not completed:
            return MountDiagnosis(
                supported=True,
                completed_downloads=0,
                mount_has_files=True,
                client_downloads_dir=client_dir,
            )
        # Resolve a small sample under the mount - the cheap get_file_path steps hit
        # first for a correct mount, so only a misconfigured one pays the walk cost.
        sample = completed[: self._DIAGNOSIS_SAMPLE]
        resolvable = 0
        for transfer in sample:
            try:
                located = await self.get_file_path(
                    TaskHandle(source="soulseek", username=transfer.username),
                    transfer.filename,
                    transfer.size or None,
                )
            except Exception:  # noqa: BLE001 - a diagnostic must never raise
                located = None
            if located is not None:
                resolvable += 1
        has_files = await asyncio.to_thread(self._mount_has_any_file)
        return MountDiagnosis(
            supported=True,
            completed_downloads=len(completed),
            mount_has_files=has_files,
            resolvable_downloads=resolvable,
            sampled_downloads=len(sample),
            client_downloads_dir=client_dir,
        )

    async def _configured_downloads_dir(self) -> str | None:
        """slskd's own ``directories.downloads`` (its in-container path), best-effort -
        used only to show the user where slskd saves so they can match it to the mount."""
        try:
            options = await self._client.get_options()
        except Exception:  # noqa: BLE001 - a diagnostic must never raise
            return None
        return options.directories.downloads or None

    def _mount_has_any_file(self) -> bool:
        """Whether the downloads mount holds any file (bounded DFS, stops at the first
        hit). An unreadable or wrong-path mount returns False - that is the signal.
        Sync filesystem I/O; the caller offloads it off the event loop."""
        try:
            stack = [self._downloads_mount]
            seen = 0
            while stack:
                for entry in stack.pop().iterdir():
                    seen += 1
                    if seen > 5000:
                        return True  # clearly not empty
                    if entry.is_file():
                        return True
                    if entry.is_dir():
                        stack.append(entry)
        except OSError:
            return False
        return False

    async def _run_search(
        self, query: str, timeout: float
    ) -> list[DownloadSearchResult]:
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
        -> artist. The broadest rung relies on the preflight scorer to narrow the
        larger result set back down - by containment title matching, and auto-accept
        additionally requires the artist to be named in a candidate's remote path
        (title tokens alone are NOT enough: a broad rung once matched a wrong-artist
        soundtrack on the bare token "arrival", 2026-07-05 incident).

        Each rung is followed by a blocked-artist variant with the artist's
        first letters wildcarded (see ``_wildcard_artist``). Exact goes first
        because wildcards degrade matching on some clients; the wildcard
        sibling comes before broadening so a blocked artist still gets the
        most specific query that can return anything."""
        wc = SlskdRepository._wildcard_artist(SlskdRepository._sanitize_query(artist))
        return SlskdRepository._dedupe_queries(
            [
                SlskdRepository._build_album_query(artist, album, year),
                SlskdRepository._build_album_query(wc, album, year),
                SlskdRepository._build_album_query(artist, album, None),
                SlskdRepository._build_album_query(wc, album, None),
                SlskdRepository._sanitize_query(artist),
                wc,
            ]
        )

    @staticmethod
    def _track_query_ladder(artist: str, track: str, album: str | None) -> list[str]:
        """Most-specific-first track queries: artist+track+album -> artist+track.
        Keeps the track title at every rung so the TrackMatcher can match.
        Wildcard blocked-artist variants interleave as in ``_album_query_ladder``."""
        wc = SlskdRepository._wildcard_artist(SlskdRepository._sanitize_query(artist))
        return SlskdRepository._dedupe_queries(
            [
                SlskdRepository._build_track_query(artist, track, album),
                SlskdRepository._build_track_query(wc, track, album),
                SlskdRepository._build_track_query(artist, track, None),
                SlskdRepository._build_track_query(wc, track, None),
            ]
        )

    @staticmethod
    def _wildcard_artist(artist: str) -> str:
        """Blocked-artist workaround: Soulseek's server filters searches that
        contain certain artist terms (DMCA), returning 0 results no matter what
        else is in the query ("Enter Shikari" -> nothing). Replacing the first
        letter of each word with Soulseek's leading wildcard defeats the filter
        while matching the same files ("*nter *hikari" -> lots). An apostrophe
        right after the first letter is absorbed into the wildcard (D'Angelo ->
        *Angelo) so matching no longer depends on the peer's apostrophe form."""
        return " ".join(SlskdRepository._wildcard_word(w) for w in artist.split())

    @staticmethod
    def _wildcard_word(word: str) -> str:
        if not word[:1].isalpha():
            return word
        rest = word[1:]
        if rest[:1] in ("'", "’"):
            rest = rest[1:]
        # keep short words exact: "*" plus a lone char matches far too much
        return f"*{rest}" if len(rest) >= 2 else word

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
        ``AC-DC`` / ``Jay-Z``. Typographic apostrophes are normalised to the
        straight ASCII form: MusicBrainz metadata uses ’ but shared files
        are almost always named with ``'``."""
        query = re.sub(r"[‘’‛ʼ]", "'", query)
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
                        extension=SlskdRepository._extension_from_filename(
                            file.filename
                        ),
                        bitrate=file.bit_rate,
                        bit_depth=file.bit_depth,
                        sample_rate=file.sample_rate,
                        duration=file.length,
                        has_free_slot=resp.has_free_upload_slot,
                        upload_speed=resp.upload_speed or 0,
                        queue_length=resp.queue_length,
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
    def _accepted_filenames(
        result: SlskdEnqueueResponse, requested: list[str]
    ) -> list[str]:
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
        self, handle: TaskHandle, transfers: list[SlskdTransfer]
    ) -> DownloadTaskStatus:
        files_total = len(handle.filenames)
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
            elif flags & {
                "errored",
                "cancelled",
                "failed",
                "rejected",
                "timedout",
                "aborted",
            }:
                # 'aborted' (a "Completed, Aborted" transfer) is terminal-failed, not active:
                # without it the file never reaches a terminal count and the task waits out
                # the full queued_timeout (~2h) instead of failing over on the next poll.
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
            matched_transfers=len(transfers),
        )
