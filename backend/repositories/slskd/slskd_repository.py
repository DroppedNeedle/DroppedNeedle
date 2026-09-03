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
import os
import re
import unicodedata
from datetime import datetime, timezone
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
_NO_TIMESTAMP = datetime.min.replace(tzinfo=timezone.utc)
_MAX_WALK_ENTRIES = 10_000


def _normalised_filename(value: str) -> str:
    """Return the NFC form used for filename comparisons only."""
    return unicodedata.normalize("NFC", value)


def _exact_transfer_path(value: str) -> str:
    """Return a transfer path key with separators normalised, but not Unicode."""
    return value.replace("\\", "/")


def _normalised_path(value: str) -> str:
    """Canonical comparison key for a path reported by slskd."""
    return _normalised_filename(_exact_transfer_path(value))

_FUZZY_DASH_SPLIT = re.compile(r"\s*[-\u2013\u2014_]\s*")
_FUZZY_TRACK_FIND = re.compile(r"\b(\d{1,3})\b")
_FUZZY_LEADING_TRACK = re.compile(r"^(\d{1,3})\b[.\-_\s]*")


def _fuzzy_file_key(basename: str) -> tuple[int | None, str, str]:
    """Split a download basename into (track_number, title_core, extension).

    Local fallback for ``_locate_file`` steps 8-9 (issue #229): peers advertise a
    flat ``Artist - Album - NN - Title`` name while slskd files the download as
    ``NN. Title`` inside an album folder. The name is NFC-normalised then
    casefolded; an ``Artist - Album`` prefix is stripped up to a standalone
    track-number segment (falling back to the first ``NN`` token); a leading
    track token (``^\\d{1,3}\\b`` over ``[.-_\\s]*`` separators) is split off and
    compared numerically; the title core drops every non-alphanumeric so only
    separator/punctuation drift remains invisible.
    """
    normalised = unicodedata.normalize("NFC", basename).casefold()
    stem, dot, ext = normalised.rpartition(".")
    if not dot or not stem:
        stem, ext = normalised, ""
    remainder = stem
    segments = _FUZZY_DASH_SPLIT.split(stem)
    for index, segment in enumerate(segments):
        if re.fullmatch(r"\d{1,3}\.?", segment.strip()):
            remainder = " - ".join(segments[index:])
            break
    else:
        found = _FUZZY_TRACK_FIND.search(stem)
        if found:
            remainder = stem[found.start(1):]
    remainder = remainder.strip()
    track: int | None = None
    title_part = remainder
    leading = _FUZZY_LEADING_TRACK.match(remainder)
    if leading:
        track = int(leading.group(1))
        title_part = remainder[leading.end():]
    core = "".join(ch for ch in title_part if ch.isalnum())
    return track, core, ext


def _fuzzy_keys_match(
    expected: tuple[int | None, str, str], candidate_name: str
) -> bool:
    """Return True when an on-disk basename names the expected download fuzzily.

    Track tokens must agree when both sides carry one, the extensions must be
    equal, both title cores must be non-trivial, and the shorter core must be
    contained in the longer one (either direction).
    """
    expected_track, expected_core, expected_ext = expected
    candidate_track, candidate_core, candidate_ext = _fuzzy_file_key(candidate_name)
    if expected_ext != candidate_ext:
        return False
    if (
        expected_track is not None
        and candidate_track is not None
        and expected_track != candidate_track
    ):
        return False
    if not expected_core or not candidate_core:
        return False
    short, long = sorted((expected_core, candidate_core), key=len)
    if len(short) < 2:
        return False
    return short in long


class _EntryBudget:
    """Shared cap for all normalized fallback directory entries."""

    __slots__ = ("remaining",)

    def __init__(self, limit: int) -> None:
        self.remaining = limit

    def take(self) -> bool:
        if self.remaining <= 0:
            return False
        self.remaining -= 1
        return True

    @property
    def exhausted(self) -> bool:
        return self.remaining <= 0


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
        incomplete_mount: Path | None = None,
    ):
        self._client = client
        self._url = url
        self._api_key = api_key
        self._downloads_mount = Path(downloads_mount)
        # Optional second mount for slskd's incomplete dir (#292). None disables the
        # partial fallback entirely; never consulted by get_file_path, only by
        # locate_partial. Stored unresolved; each lookup resolves it fresh so a
        # symlinked root cannot escape confinement (escape-in).
        self._incomplete_mount = Path(incomplete_mount) if incomplete_mount else None
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
        matched = self._match_transfers(handle, transfers)
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
        matched = self._match_transfers(handle, transfers)
        ok = True
        for transfer in matched:
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
        """Resolve a finished transfer inside the mounted slskd downloads directory.

        Exact spelling is always tried before a normalized alias, and a fuzzy
        track-number/title fallback (steps 8-9) runs last.  Alias lookup is
        deliberately bounded and fail-closed: it is confined to the resolved mount,
        accepts regular files only, checks a positive expected size, and returns a
        path only when exactly one matching on-disk file exists. A single
        normalized alias still resolves when the expected size mismatches; size
        rejection belongs to the verifier (SIZE_MISMATCH), not the locator.
        """
        raw_parts = re.split(r"[\\/]", remote_filename)
        if any(part == ".." for part in raw_parts):
            return None
        parts = [part for part in raw_parts if part and part != "."]
        if not parts:
            return None
        try:
            mount = self._downloads_mount.resolve()
        except (OSError, RuntimeError):
            return None
        basename = parts[-1]
        normalised_basename = _normalised_filename(basename)
        expected_size = size if size is not None and size > 0 else None

        def _within_mount(candidate: Path) -> Path | None:
            try:
                resolved = candidate.resolve()
            except (OSError, RuntimeError):
                return None
            if not resolved.is_relative_to(mount):
                logger.warning(
                    "slskd path escapes the downloads mount: %r", remote_filename
                )
                return None
            return resolved

        def _find_direct_exact(directory: Path) -> Path | None:
            candidate = _within_mount(directory / basename)
            if candidate is not None and candidate.is_file():
                return candidate
            return None

        def _name_matches(entry: Path) -> bool:
            return entry.name == basename

        # 1. slskd's common layout: {mount}/{leaf remote folder}/{filename}.
        if len(parts) >= 2:
            leaf = _find_direct_exact(mount / parts[-2])
            if leaf is not None:
                return leaf
        # 2. Flat layout: {mount}/{filename}.
        flat = _find_direct_exact(mount)
        if flat is not None:
            return flat
        # 3. Peers that file by username: walk {mount}/{username}/ at any depth.
        # (covers {username}/{file} and {username}/{album}/{file}). Scoped so a
        # same-named track from another peer cannot be picked up.
        user_root = _within_mount(mount / username) if username else None
        if user_root is not None and user_root.is_dir():
            hit = self._walk_find(user_root, mount, _name_matches)
            if hit is not None:
                return hit
        # 4. slskd may have sanitised the folder name - scan one level down for it.
        try:
            for child in sorted(mount.iterdir(), key=lambda path: path.name):
                child_root = _within_mount(child)
                if child_root is None or not child_root.is_dir():
                    continue
                cand = _find_direct_exact(child_root)
                if cand is not None:
                    return cand
        except (OSError, RuntimeError) as exc:
            logger.warning("Could not scan downloads mount %s: %s", mount, exc)
        # 5. Last resort: slskd may have sanitised the filename.  An exact byte-size
        # match under the peer's folder recovers it; this pre-existing fallback remains
        # peer-scoped because a size-only walk across peers is unsafe.
        if expected_size is not None and user_root is not None and user_root.is_dir():

            def _matches_size(entry: Path) -> bool:
                try:
                    return entry.stat().st_size == expected_size
                except OSError:
                    return False

            hit = self._walk_find(user_root, mount, _matches_size)
            if hit is not None:
                return hit

        # 6. Whole-mount exact-name fallback for a file nested deeper than the cheap
        # steps look. Validate byte size when known.
        def _name_size_match(entry: Path) -> bool:
            if not _name_matches(entry):
                return False
            if expected_size is None:
                return True
            try:
                return entry.stat().st_size == expected_size
            except OSError:
                return False

        hit = self._walk_find(mount, mount, _name_size_match)
        if hit is not None:
            return hit

        # 7. NFC alias fallback. Every normalized phase shares this one budget. The
        # peer scope is attempted before the whole mount so an alias cannot cross peers
        # merely because an unrelated same-sized file happens to be encountered first.
        budget = _EntryBudget(_MAX_WALK_ENTRIES)

        def _find_normalised_in_directory(
            directory: Path,
        ) -> tuple[Path | None, str | None, int]:
            """Return one immediate normalized alias, or ambiguity/exhaustion.

            Returns (hit, fail_kind, fail_count): fail_kind is None to keep
            looking, "ambiguous" (fail_count = observed alias count) or
            "budget" (fail_count unused).
            """
            root = _within_mount(directory)
            if root is None or not root.is_dir():
                return None, None, 0
            matches: set[Path] = set()
            try:
                for entry in root.iterdir():
                    if not budget.take():
                        return None, "budget", 0
                    resolved = _within_mount(entry)
                    if resolved is None or not resolved.is_file():
                        continue
                    if _normalised_filename(entry.name) != normalised_basename:
                        continue
                    if expected_size is not None:
                        try:
                            if resolved.stat().st_size != expected_size:
                                continue
                        except OSError:
                            continue
                    matches.add(resolved)
                    if len(matches) > 1:
                        return None, "ambiguous", len(matches)
            except (OSError, RuntimeError):
                return None, None, 0
            return (next(iter(matches)) if matches else None), None, 0

        def _walk_find_normalised(root: Path) -> tuple[Path | None, str | None, int]:
            """Find one normalized alias under root, confined and loop-safe.

            Returns (hit, fail_kind, fail_count) like
            `_find_normalised_in_directory`. When the size-gated set is empty
            but the expected size is known, the already-collected in-memory
            alias set is retried without the size gate: exactly one alias
            resolves (size rejection is then the verifier's job), zero stays
            not-found, several fail closed as ambiguous. No new walks, no
            extra budget entries. Every candidate stays `_within_mount` +
            regular-file gated.
            """
            resolved_root = _within_mount(root)
            if resolved_root is None or not resolved_root.is_dir():
                return None, None, 0
            stack = [resolved_root]
            seen_dirs: set[Path] = set()
            matches: set[Path] = set()
            ungated: set[Path] = set()
            while stack:
                current = stack.pop()
                current = _within_mount(current)
                if current is None or not current.is_dir() or current in seen_dirs:
                    continue
                seen_dirs.add(current)
                try:
                    entries = current.iterdir()
                    for entry in entries:
                        if not budget.take():
                            return None, "budget", 0
                        resolved = _within_mount(entry)
                        if resolved is None:
                            continue
                        if resolved.is_dir():
                            stack.append(resolved)
                            continue
                        if not resolved.is_file():
                            continue
                        if _normalised_filename(entry.name) != normalised_basename:
                            continue
                        if expected_size is None:
                            matches.add(resolved)
                        else:
                            ungated.add(resolved)
                            try:
                                if resolved.stat().st_size != expected_size:
                                    continue
                            except OSError:
                                continue
                            matches.add(resolved)
                        if len(matches) > 1:
                            return None, "ambiguous", len(matches)
                except (OSError, RuntimeError):
                    continue
            if matches:
                return next(iter(matches)), None, 0
            if expected_size is not None:
                if len(ungated) == 1:
                    return next(iter(ungated)), None, 0
                if len(ungated) > 1:
                    return None, "ambiguous", len(ungated)
            return None, None, 0

        def _log_unlocatable(kind: str, count: int = 0) -> None:
            """Log a fail-closed miss with the minimal shape: basename, size (or
            "unknown"), and the top-level entry count only. Ambiguity adds the
            observed candidate count; budget exhaustion adds a flag. Never
            candidate paths, usernames, hosts, secrets, remote full paths, or
            exception text."""
            try:
                top_level = sum(1 for _ in mount.iterdir())
            except (OSError, RuntimeError):
                top_level = -1
            if kind == "budget":
                logger.warning(
                    "slskd file not locatable on the downloads mount: %s (%s bytes); "
                    "%d top-level entries under the mount - entry budget exhausted, "
                    "refusing to guess",
                    basename,
                    size if size else "unknown",
                    top_level,
                )
            elif kind == "ambiguous":
                logger.warning(
                    "slskd file not locatable on the downloads mount: %s (%s bytes); "
                    "%d top-level entries under the mount - ambiguous=%d, "
                    "refusing to guess",
                    basename,
                    size if size else "unknown",
                    top_level,
                    count,
                )
            else:
                logger.warning(
                    "slskd file not locatable on the downloads mount: %s (%s bytes); "
                    "%d top-level entries under the mount - the on-disk layout may nest deeper "
                    "or sanitise names beyond what get_file_path handles",
                    basename,
                    size if size else "unknown",
                    top_level,
                )

        # Keep direct-directory aliases ahead of recursive fallback; these are the
        # layouts slskd most commonly produces.
        if len(parts) >= 2:
            hit, fail_kind, fail_count = _find_normalised_in_directory(mount / parts[-2])
            if hit is not None:
                return hit
            if fail_kind is not None:
                _log_unlocatable(fail_kind, fail_count)
                return None
        hit, fail_kind, fail_count = _find_normalised_in_directory(mount)
        if hit is not None:
            return hit
        if fail_kind is not None:
            _log_unlocatable(fail_kind, fail_count)
            return None

        if user_root is not None and user_root.is_dir():
            hit, fail_kind, fail_count = _walk_find_normalised(user_root)
            if hit is not None:
                return hit
            if fail_kind is not None:
                _log_unlocatable(fail_kind, fail_count)
                return None

        hit, fail_kind, fail_count = _walk_find_normalised(mount)
        if hit is not None:
            return hit
        if fail_kind is not None:
            _log_unlocatable(fail_kind, fail_count)
            return None
        # 8-9. Fuzzy basename fallback for peers that advertise a flat
        # "Artist - Album - NN - Title" name while slskd files the download as
        # "NN. Title" inside an album folder (issue #229). Fail-closed like the
        # NFC phases: confined to the mount, regular files only, sharing the
        # same budget, and a hit only when exactly one candidate matches
        # (ambiguity or budget exhaustion returns None). The peer scope runs
        # first so a same-titled file from another peer cannot shadow it; the
        # mount-wide sweep stays behind a mandatory exact byte-size gate.
        expected_fuzzy_key = _fuzzy_file_key(basename)

        def _walk_find_fuzzy(
            root: Path, require_size: bool
        ) -> tuple[Path | None, str | None, int]:
            """Collect fuzzy basename matches under root, confined and loop-safe.

            Returns (hit, fail_kind, fail_count) like
            `_find_normalised_in_directory`.
            """
            resolved_root = _within_mount(root)
            if resolved_root is None or not resolved_root.is_dir():
                return None, None, 0
            stack = [resolved_root]
            seen_dirs: set[Path] = set()
            matches: set[Path] = set()
            while stack:
                current = stack.pop()
                current = _within_mount(current)
                if current is None or not current.is_dir() or current in seen_dirs:
                    continue
                seen_dirs.add(current)
                try:
                    entries = current.iterdir()
                    for entry in entries:
                        if not budget.take():
                            return None, "budget", 0
                        resolved = _within_mount(entry)
                        if resolved is None:
                            continue
                        if resolved.is_dir():
                            stack.append(resolved)
                            continue
                        if not resolved.is_file():
                            continue
                        if not _fuzzy_keys_match(expected_fuzzy_key, entry.name):
                            continue
                        if expected_size is not None:
                            try:
                                if resolved.stat().st_size != expected_size:
                                    continue
                            except OSError:
                                continue
                        elif require_size:
                            continue
                        matches.add(resolved)
                        if len(matches) > 1:
                            return None, "ambiguous", len(matches)
                except (OSError, RuntimeError):
                    continue
            return (next(iter(matches)) if matches else None), None, 0

        # 8. Peer-scoped fuzzy: size gates only when the expected size is known.
        if user_root is not None and user_root.is_dir():
            hit, fail_kind, fail_count = _walk_find_fuzzy(user_root, require_size=False)
            if hit is not None:
                return hit
            if fail_kind is not None:
                _log_unlocatable(fail_kind, fail_count)
                return None

        # 9. Mount-wide fuzzy: meaningless without a size gate, so skipped
        # entirely when the expected size is unknown.
        if expected_size is not None:
            hit, fail_kind, fail_count = _walk_find_fuzzy(mount, require_size=True)
            if hit is not None:
                return hit
            if fail_kind is not None:
                _log_unlocatable(fail_kind, fail_count)
                return None

        _log_unlocatable("none")
        return None

    async def locate_partial(
        self, handle: TaskHandle, remote_filename: str, size: int | None = None
    ) -> Path | None:
        """Basename-keyed partial fallback confined to the incomplete mount, OFF the
        event loop.

        Never called by ``get_file_path`` (which stays byte-identical): only the
        verifier's retry-signal path consults it, and only for subset imports.
        Returns the single matching partial file, or None when the mount is
        unset/unusable, the name is absent, or several same-named partials exist.
        """
        return await asyncio.to_thread(
            self._locate_partial, handle.username, remote_filename, size
        )

    def _locate_partial(
        self, username: str, remote_filename: str, size: int | None = None
    ) -> Path | None:
        """Find stranded partial bytes by exact basename (then NFC alias at most).

        ``username`` is intentionally ignored: slskd's incomplete layout
        (``incomplete/<album>/<file>``) is not username-scoped, so the safe key
        is the basename plus exactly-one confinement. No fuzzy phase, no
        size-only phase. The expected size is recorded for the log line only:
        partial files are short by definition, so an equality gate would never
        hit. An unknown size still allows the direct probes but skips the
        recursive sweep (the phase-9 require_size discipline).
        """
        root_setting = self._incomplete_mount
        if root_setting is None:
            return None
        raw_parts = re.split(r"[\\/]", remote_filename)
        if any(part == ".." for part in raw_parts):
            return None
        parts = [part for part in raw_parts if part and part != "."]
        if not parts:
            return None
        try:
            incomplete = root_setting.resolve()
        except (OSError, RuntimeError):
            return None
        if not incomplete.is_dir() or not os.access(incomplete, os.R_OK):
            return None
        basename = parts[-1]
        normalised_basename = _normalised_filename(basename)
        size_label = size if size else "unknown"

        def _within_incomplete(candidate: Path) -> Path | None:
            try:
                resolved = candidate.resolve()
            except (OSError, RuntimeError):
                return None
            if not resolved.is_relative_to(incomplete):
                logger.warning(
                    "slskd path escapes the incomplete mount: %r", basename
                )
                return None
            return resolved

        def _log_partial(kind: str, count: int = 0) -> None:
            """Fail-closed log with the minimal shape: basename, size (or
            "unknown"), and the observed candidate count. Never paths,
            usernames, hosts, secrets, or exception text."""
            if kind == "ambiguous":
                logger.warning(
                    "slskd partial file not usable from the incomplete mount: %s "
                    "(%s bytes) - ambiguous=%d, refusing to guess",
                    basename,
                    size_label,
                    count,
                )
            elif kind == "budget":
                logger.warning(
                    "slskd partial file not usable from the incomplete mount: %s "
                    "(%s bytes) - entry budget exhausted, refusing to guess",
                    basename,
                    size_label,
                )

        def _log_hit(path: Path) -> Path:
            try:
                on_disk = path.stat().st_size
            except OSError:
                on_disk = size_label
            logger.info(
                "slskd partial bytes found in the incomplete mount: %s (%s bytes)",
                basename,
                on_disk,
            )
            return path

        # Direct probes: the flat file and the one-level album-dir layout
        # (incomplete/<album>/<file>) slskd most commonly produces.
        direct: set[Path] = set()
        flat = _within_incomplete(incomplete / basename)
        if flat is not None and flat.is_file():
            direct.add(flat)
        try:
            children = sorted(incomplete.iterdir(), key=lambda path: path.name)
        except (OSError, RuntimeError):
            children = []
        for child in children:
            child_root = _within_incomplete(child)
            if child_root is None or not child_root.is_dir():
                continue
            cand = _within_incomplete(child_root / basename)
            if cand is not None and cand.is_file():
                direct.add(cand)
        if len(direct) > 1:
            _log_partial("ambiguous", len(direct))
            return None
        if direct:
            return _log_hit(next(iter(direct)))

        if size is None or size <= 0:
            logger.debug(
                "slskd partial file not in the incomplete mount: %s (%s bytes)",
                basename,
                size_label,
            )
            return None

        # Recursive exact-then-NFC sweep with its own budget (never shared with
        # the complete-mount phases, which may already be exhausted). Exactly
        # one basename match resolves; several fail closed as ambiguous.
        budget = _EntryBudget(_MAX_WALK_ENTRIES)
        exact: set[Path] = set()
        aliased: set[Path] = set()
        stack = [incomplete]
        seen_dirs: set[Path] = set()
        while stack:
            current = stack.pop()
            current = _within_incomplete(current)
            if current is None or not current.is_dir() or current in seen_dirs:
                continue
            seen_dirs.add(current)
            try:
                entries = current.iterdir()
                for entry in entries:
                    if not budget.take():
                        _log_partial("budget")
                        return None
                    resolved = _within_incomplete(entry)
                    if resolved is None:
                        continue
                    if resolved.is_dir():
                        stack.append(resolved)
                        continue
                    if not resolved.is_file():
                        continue
                    if entry.name == basename:
                        exact.add(resolved)
                        if len(exact) > 1:
                            _log_partial("ambiguous", len(exact))
                            return None
                    elif _normalised_filename(entry.name) == normalised_basename:
                        aliased.add(resolved)
            except (OSError, RuntimeError):
                continue
        if exact:
            return _log_hit(next(iter(exact)))
        if len(aliased) > 1:
            _log_partial("ambiguous", len(aliased))
            return None
        if aliased:
            return _log_hit(next(iter(aliased)))
        logger.debug(
            "slskd partial file not in the incomplete mount: %s (%s bytes)",
            basename,
            size_label,
        )
        return None

    @staticmethod
    def _walk_find(root: Path, mount: Path, predicate) -> Path | None:
        """Find the first matching regular file under ``root``.

        Directory traversal is bounded, confined to the resolved mount, and keyed by
        resolved directory paths so in-mount symlink loops cannot revisit forever.
        """
        try:
            mount = mount.resolve()
            root = root.resolve()
        except (OSError, RuntimeError):
            return None
        if not root.is_relative_to(mount) or not root.is_dir():
            return None
        stack = [root]
        seen_dirs: set[Path] = set()
        seen = 0
        try:
            while stack:
                current = stack.pop().resolve()
                if (
                    not current.is_relative_to(mount)
                    or not current.is_dir()
                    or current in seen_dirs
                ):
                    continue
                seen_dirs.add(current)
                for entry in current.iterdir():
                    seen += 1
                    if seen > _MAX_WALK_ENTRIES:
                        return None
                    resolved = entry.resolve()
                    if not resolved.is_relative_to(mount):
                        continue
                    if resolved.is_dir():
                        stack.append(resolved)
                        continue
                    if not resolved.is_file() or not predicate(entry):
                        continue
                    return resolved
        except (OSError, RuntimeError):
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
            mount = self._downloads_mount.resolve()
            if not mount.is_dir():
                return False
            stack = [mount]
            seen_dirs: set[Path] = set()
            seen = 0
            while stack:
                current = stack.pop().resolve()
                if (
                    not current.is_relative_to(mount)
                    or not current.is_dir()
                    or current in seen_dirs
                ):
                    continue
                seen_dirs.add(current)
                for entry in current.iterdir():
                    seen += 1
                    if seen > 5000:
                        return True  # clearly not empty
                    resolved = entry.resolve()
                    if not resolved.is_relative_to(mount):
                        continue
                    if resolved.is_file():
                        return True
                    if resolved.is_dir():
                        stack.append(resolved)
        except (OSError, RuntimeError):
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

    @staticmethod
    def _transfer_recency(transfer: SlskdTransfer) -> datetime:
        """Best-effort recency key for one transfer record: RequestedAt first,
        falling back to StartedAt (requestedAt is absent/mixed across slskd
        versions, PR #222). Absent or unparseable values rank as the oldest
        possible instant, so any parseable timestamp beats a missing one; naive
        timestamps read as UTC. slskd's ``id`` is a GUID - not monotonic - so it
        carries no recency signal."""
        for value in (transfer.requested_at, transfer.started_at):
            if not value:
                continue
            try:
                parsed = datetime.fromisoformat(value)
            except ValueError:
                continue
            return parsed if parsed.tzinfo else parsed.replace(tzinfo=timezone.utc)
        return _NO_TIMESTAMP

    @staticmethod
    def _match_transfers(
        handle: TaskHandle,
        transfers: list[SlskdTransfer],
    ) -> list[SlskdTransfer]:
        """Match transfer records to handle filenames without merging spellings.

        Each handle filename claims all records with its exact transfer path key
        (path separators normalised, Unicode unchanged). If no exact spelling is
        present, it may claim the NFC-equivalent records only when those records
        have one distinct exact spelling. A transfer record can be assigned only
        once when multiple handle filenames overlap.
        """
        exact: dict[str, list[int]] = {}
        nfc: dict[str, dict[str, list[int]]] = {}
        for index, transfer in enumerate(transfers):
            exact_key = _exact_transfer_path(transfer.filename)
            exact.setdefault(exact_key, []).append(index)
            nfc_key = _normalised_path(transfer.filename)
            nfc.setdefault(nfc_key, {}).setdefault(exact_key, []).append(index)

        assigned: set[int] = set()
        for filename in handle.filenames:
            exact_key = _exact_transfer_path(filename)
            exact_matches = exact.get(exact_key)
            if exact_matches is not None:
                assigned.update(exact_matches)

        for filename in handle.filenames:
            exact_key = _exact_transfer_path(filename)
            if exact_key in exact:
                continue
            spellings = nfc.get(_normalised_path(filename), {})
            if len(spellings) == 1:
                assigned.update(next(iter(spellings.values())))

        return [transfer for index, transfer in enumerate(transfers) if index in assigned]


    @staticmethod
    def _latest_transfer_per_file(
        transfers: list[SlskdTransfer],
    ) -> list[SlskdTransfer]:
        """Collapse records to the LATEST attempt per unique file (#131/#253):
        slskd appends one record per retry attempt, so raw counts double-count
        retried files and let a stale Succeeded row shadow a newer TimedOut/
        Errored one (and vice versa). Highest recency key wins; exact ties -
        including two untimestamped/garbage-stamped records - fall through to
        list order, where the later record wins. Filenames use exact transfer
        path keys; winners keep their original input order.
        """
        best: dict[str, tuple[datetime, int, SlskdTransfer]] = {}
        for index, transfer in enumerate(transfers):
            key = _exact_transfer_path(transfer.filename)
            recency = SlskdRepository._transfer_recency(transfer)
            incumbent = best.get(key)
            if incumbent is None or recency >= incumbent[0]:
                best[key] = (recency, index, transfer)
        return [entry[2] for entry in sorted(best.values(), key=lambda entry: entry[1])]

    def _aggregate_status(
        self, handle: TaskHandle, transfers: list[SlskdTransfer]
    ) -> DownloadTaskStatus:
        """Per-file status from matched transfer records. File-level verdicts
        (completed/failed counts, succeeded_filenames, terminal states) judge
        each file ONLY by its LATEST attempt; byte totals deliberately stay
        sum-over-all-records so cumulative progress keeps counting prior attempts.
        A "succeeded" flag only counts when the transfer moved at least ``size``
        bytes (size known positive); a short succeeded record is a truncated stub
        (#122): failed when terminal, non-terminal when still active. Unknown or
        non-positive sizes fail open to the flag verdict."""
        files_total = len(handle.filenames)
        # Byte totals stay sum-over-all-records (see docstring): each attempt's bytes
        # count toward cumulative progress, sizes likewise sum across retry attempts.
        bytes_total = sum(t.size for t in transfers)
        bytes_downloaded = sum(t.bytes_transferred for t in transfers)
        completed = 0
        failed = 0
        succeeded_filenames: list[str] = []
        has_active_transfer = False
        queue_positions: list[int] = []

        # Judge each FILE by its LATEST attempt, never by raw record counts.
        latest_per_file = SlskdRepository._latest_transfer_per_file(transfers)
        for transfer in latest_per_file:
            flags = self._state_flags(transfer.state)
            if transfer.place_in_queue is not None and transfer.place_in_queue >= 0:
                queue_positions.append(transfer.place_in_queue)
            if "succeeded" in flags:
                size = transfer.size
                size_known = isinstance(size, (int, float)) and size > 0
                moved = transfer.bytes_transferred
                moved_value = moved if isinstance(moved, (int, float)) else 0
                if size_known and moved_value < size:
                    # Truncated stub flagged succeeded: never importable. Still
                    # active -> stay non-terminal; terminal -> fail over/retries.
                    if flags & {"inprogress", "initializing"}:
                        has_active_transfer = True
                    else:
                        failed += 1
                else:
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
            and (completed + failed) == len(latest_per_file)
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
            queue_position_start=min(queue_positions) if queue_positions else None,
            queue_position_end=max(queue_positions) if queue_positions else None,
        )
