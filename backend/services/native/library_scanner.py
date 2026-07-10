"""LibraryScanner - walk library paths, identify files, populate library_files.

Orchestrates tiered identification over the audio files under the configured
library paths and writes the outcome through ``LibraryManager``:

- **Tier 1** - MBIDs already in the file's tags (confidence 1.0).
- **Tier 2** - fuzzy text match against MusicBrainz (confidence >= 0.85).
- **Tier 3** - AcoustID fingerprint -> recording -> release group (score >= 0.70).
- **Tier 4** - no confident match: queued to ``manual_review_queue``.

Supports resume (the ``scan_progress`` ledger - AUD-4), cooperative cancellation
(``asyncio.Event``), incremental skip (unchanged mtime+size), and a post-walk
soft-delete reconcile. Progress is published on the ``library:scan`` SSE channel.

Singleton (one per app): the cancel route and the running scan share the same
instance and its ``_cancel`` event.
"""

import asyncio
import logging
import os
import re
import time
from collections import Counter
from pathlib import Path
from typing import Awaitable, Callable, NamedTuple, TYPE_CHECKING

import msgspec

from core.exceptions import ResourceNotFoundError, ValidationError
from infrastructure.msgspec_fastapi import AppStruct
from services.native.album_matcher import LocalTrack
from services.native.filename_parser import parse_names_from_path
from services.native.library_manager import _AUDIO_SUFFIXES
from services.native.musicbrainz_matcher import TargetAlbum

if TYPE_CHECKING:
    from infrastructure.audio.fingerprinter import AudioFingerprinter
    from infrastructure.audio.tagger import AudioTagger
    from infrastructure.persistence.scan_state_store import ScanStateStore
    from infrastructure.sse_publisher import SSEPublisher
    from models.audio import AudioInfo, AudioTag, FingerprintResult
    from services.native.album_matcher import AlbumIdentifier, AlbumMatch
    from services.native.library_manager import LibraryManager, LibraryTrack
    from services.native.musicbrainz_matcher import MusicBrainzMatcher

logger = logging.getLogger(__name__)

_SCAN_CHANNEL = "library:scan"
_TEXT_MATCH_THRESHOLD = 0.85
_FINGERPRINT_SCORE_THRESHOLD = 0.70
_LEDGER_BATCH_SIZE = 100
_PROGRESS_INTERVAL_SECONDS = 2.0
# Bounded parallelism for the local-only stat + tag-read stage (profiled: the file
# open dominates a scan's local cost). Identification stays strictly sequential.
_TAG_READ_CONCURRENCY = 8
# Folders larger than this are treated as a flat dump, not an album.
_MAX_ALBUM_FILES = 60
_ARTIST_RECONCILE_PASSES = 8
_ARTIST_BREAKER_WAIT_S = 65.0
# Re-attribution guards (ScannerAlbumIdentity plan, Phase 1). A prior attribution from a
# download, or scored at least this high, is an "anchor" the scan won't lightly overwrite.
_STICKY_CONFIDENCE = 0.85
_STICKY_MARGIN = 0.05
_COMP_OR_LIVE = frozenset({"compilation", "live"})
# Phase 2: a folder file the matched release didn't map to a track is kept as an album
# member (by folder cohesion) with a blank recording, at this modest confidence - enough
# to own it, low enough not to become a sticky anchor itself.
_UNMAPPED_ALBUM_CONFIDENCE = 0.5


class _FileEntry(NamedTuple):
    """A file needing identification, with its tags already read."""

    path: Path
    tag: "AudioTag"
    info: "AudioInfo"
    mtime: float


class TieredMatchResult(AppStruct):
    matched: bool
    tier: int
    confidence: float = 0.0
    release_group_mbid: str | None = None
    release_mbid: str | None = None
    recording_mbid: str | None = None
    # How far ID got - labels a Tier-4 review row.
    fingerprint_attempted: bool = False
    fingerprint: str | None = None
    fingerprint_score: float | None = None


class ScanStats(AppStruct):
    matched: int = 0
    unmatched: int = 0
    errored: int = 0


class LibraryScanner:
    def __init__(
        self,
        audio_tagger: "AudioTagger",
        fingerprinter: "AudioFingerprinter",
        mb_matcher: "MusicBrainzMatcher",
        album_identifier: "AlbumIdentifier",
        library_manager: "LibraryManager",
        scan_state_store: "ScanStateStore",
        event_bus: "SSEPublisher",
        invalidate_albums: "Callable[[set[str]], Awaitable[None]] | None" = None,
    ) -> None:
        self._tagger = audio_tagger
        self._fingerprinter = fingerprinter
        self._mb_matcher = mb_matcher
        self._album_identifier = album_identifier
        self._library = library_manager
        self._state = scan_state_store
        self._events = event_bus
        self._cancel = asyncio.Event()
        self._running = False
        # Release groups a scan/re-identify re-attributed - their cached album pages are
        # stale and get busted when the run finishes (so the page reflects the new identity
        # without a manual refresh). Reset per run; ``None`` invalidator = no-op (tests).
        self._invalidate_albums = invalidate_albums
        self._changed_rgs: set[str] = set()

    def request_cancel(self) -> None:
        """Signal a running scan to stop at the next file boundary."""
        self._cancel.set()

    # -- admin file operations (Phase 5) --
    # Reuse the scanner's collaborators to (re-)identify single files outside a
    # full scan, raising mapped domain exceptions so routes stay thin.

    async def read_track_tags(self, file_id: str) -> "AudioTag":
        """Read a library file's current tags from disk (admin tag-editor prefill).
        Returns the full ``AudioTag`` so the editor never silently drops a field
        (e.g. genre) that the slim DB projection doesn't carry."""
        row = await self._library.get_file_row_by_id(file_id)
        if row is None:
            raise ResourceNotFoundError("Library file not found")
        path = Path(row["file_path"])
        if not path.exists():
            raise ValidationError("The audio file is no longer present on disk")
        try:
            tag, _info = await asyncio.to_thread(self._tagger.read_tags, path)
        except Exception as exc:  # noqa: BLE001 - surface as a 400, never a 500
            logger.warning("Cannot read tags from %s: %s", path, exc)
            raise ValidationError("Could not read the audio file") from exc
        return tag

    async def update_track_tags(self, file_id: str, new_tag: "AudioTag") -> "LibraryTrack":
        """Write an admin's edited tags to the file and refresh its DB row.

        Preserves the row's provenance (``source``/``confidence``) and its
        compilation flag (not an editable field). Returns the refreshed track."""
        row = await self._library.get_file_row_by_id(file_id)
        if row is None:
            raise ResourceNotFoundError("Library file not found")
        if not new_tag.musicbrainz_release_group_id:
            raise ValidationError("A release group MBID is required")
        path = Path(row["file_path"])
        if not path.exists():
            raise ValidationError("The audio file is no longer present on disk")
        tag_to_write = msgspec.structs.replace(
            new_tag, compilation=bool(row.get("is_compilation"))
        )
        try:
            await asyncio.to_thread(self._tagger.write_mb_tags, path, tag_to_write)
            tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
        except Exception as exc:  # noqa: BLE001 - surface as a 400, never a 500
            logger.warning("Tag write/read failed for %s: %s", path, exc)
            raise ValidationError("Could not write tags to the audio file") from exc
        await self._library.upsert_file(
            path,
            tag,
            info,
            # If the re-read tag dropped the RG, fall back to the validated
            # request value so a round-trip quirk can't raise a raw 500.
            release_group_mbid=(
                tag.musicbrainz_release_group_id or new_tag.musicbrainz_release_group_id
            ),
            release_mbid=tag.musicbrainz_release_id,
            recording_mbid=tag.musicbrainz_recording_id,
            confidence=float(row.get("confidence") or 1.0),
            source=str(row.get("source") or "scan"),
        )
        updated = await self._library.get_track_by_id(file_id)
        if updated is None:  # pragma: no cover - upsert just wrote this row
            raise ResourceNotFoundError("Library file not found")
        return updated

    async def resolve_unmatched(
        self, review_id: int, resolution: str, mbid: str | None = None
    ) -> None:
        """Resolve a manual-review row: ``accept`` (top candidate or supplied MBID),
        ``reject`` (mark resolved, no import), or ``manual_id`` (supplied MBID)."""
        row = await self._library.get_unmatched_row_by_id(review_id)
        if row is None or row.get("resolution") is not None:
            raise ResourceNotFoundError("Unmatched file not found")

        if resolution == "reject":
            await self._library.mark_unmatched_resolved(review_id, "rejected")
            return

        if resolution == "accept":
            candidates = row.get("candidate_mbids") or []
            chosen = (mbid or "").strip() or (candidates[0] if candidates else None)
            db_resolution = "accepted"
        elif resolution == "manual_id":
            chosen = (mbid or "").strip() or None
            db_resolution = "manual_id"
        else:
            raise ValidationError(
                "Unknown resolution; expected 'accept', 'reject', or 'manual_id'"
            )
        if not chosen:
            raise ValidationError("A MusicBrainz ID is required to accept this file")

        path = Path(row["file_path"])
        if not path.exists():
            raise ValidationError("The audio file is no longer present on disk")
        try:
            tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
        except Exception as exc:  # noqa: BLE001 - surface as a 400, never a 500
            logger.warning("Cannot read tags from %s: %s", path, exc)
            raise ValidationError("Could not read the audio file") from exc

        # The chosen MBID may be an AcoustID recording id (what the scanner stores
        # as a candidate) or a release-group id pasted by the admin. Resolve
        # recording->release-group when possible; otherwise treat it as the RG.
        release_group = await self._mb_matcher.resolve_recording_to_release_group(chosen)
        recording_mbid = chosen if release_group else None
        if not release_group:
            release_group = chosen

        await self._library.upsert_file(
            path,
            tag,
            info,
            release_group_mbid=release_group,
            recording_mbid=recording_mbid,
            confidence=1.0,
            source="manual_review",
        )
        await self._library.mark_unmatched_resolved(review_id, db_resolution)

    async def resolve_unmatched_batch(
        self, release_group_mbid: str, items: list[tuple[int, str | None]]
    ) -> dict:
        """Attribute many unmatched files to one known album in a single pass."""
        if not release_group_mbid:
            raise ValidationError("A release group is required")
        resolved = 0
        failed: list[dict] = []
        for review_id, recording_mbid in items:
            try:
                await self._import_unmatched_to_album(
                    review_id, release_group_mbid, recording_mbid
                )
                resolved += 1
            except (ResourceNotFoundError, ValidationError) as exc:
                failed.append({"review_id": review_id, "error": str(exc)})
            except Exception as exc:  # noqa: BLE001 - one bad file must not abort the batch
                logger.warning("Batch resolve failed for review %s: %s", review_id, exc)
                failed.append({"review_id": review_id, "error": "Could not import this file"})
        return {"resolved": resolved, "failed": failed}

    async def _import_unmatched_to_album(
        self, review_id: int, release_group_mbid: str, recording_mbid: str | None
    ) -> None:
        """Import one unmatched file against a known release group and recording."""
        row = await self._library.get_unmatched_row_by_id(review_id)
        if row is None or row.get("resolution") is not None:
            raise ResourceNotFoundError("Unmatched file not found")
        path = Path(row["file_path"])
        if not path.exists():
            raise ValidationError("The audio file is no longer present on disk")
        try:
            tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
        except Exception as exc:  # noqa: BLE001 - surface as a 400, never a 500
            logger.warning("Cannot read tags from %s: %s", path, exc)
            raise ValidationError("Could not read the audio file") from exc
        await self._library.upsert_file(
            path,
            tag,
            info,
            release_group_mbid=release_group_mbid,
            recording_mbid=recording_mbid or None,
            confidence=1.0,
            source="manual_review",
        )
        await self._library.mark_unmatched_resolved(review_id, "manual_id")

    async def rescan_album(self, release_group_mbid: str) -> int:
        """Refresh an album's ``library_files`` rows from disk (admin).

        Re-reads each file's tags + technical info and upserts; soft-deletes files
        gone from disk. Preserves the album grouping and provenance - it does NOT
        re-run MusicBrainz identification (a full re-identify is the library scan's
        job). Returns the number of files refreshed."""
        rows = await self._library.get_file_rows_for_album(release_group_mbid)
        refreshed = 0
        for row in rows:
            path = Path(row["file_path"])
            if not path.exists():
                await self._library.soft_delete_file(str(path))
                continue
            try:
                tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
            except Exception as exc:  # noqa: BLE001 - a corrupt file must not abort rescan
                logger.warning("Rescan: cannot read %s: %s", path, exc)
                continue
            await self._library.upsert_file(
                path,
                tag,
                info,
                release_group_mbid=(
                    tag.musicbrainz_release_group_id
                    or row.get("release_group_mbid")
                    or release_group_mbid
                ),
                release_mbid=tag.musicbrainz_release_id or row.get("release_mbid"),
                recording_mbid=tag.musicbrainz_recording_id or row.get("recording_mbid"),
                confidence=float(row.get("confidence") or 1.0),
                source=str(row.get("source") or "scan"),
            )
            refreshed += 1
        logger.info(
            "Rescanned album %s: %d file(s) refreshed", release_group_mbid, refreshed
        )
        return refreshed

    async def reidentify_album(self, release_group_mbid: str) -> int:
        """Force a fresh whole-folder re-identification of an album's files, ignoring the
        stability guards (anchor + stickiness + INV-1). The manual correction path
        (ScannerAlbumIdentity Phase 4 / R-CORRECT): the deliberate way to fix an album the
        scan settled on the wrong release group, since stability otherwise makes a confident
        attribution sticky. Returns the number of files re-identified."""
        # A prior cancelled scan leaves the shared _cancel event set; clear it so a manual
        # re-identify isn't a silent no-op (mirrors _run_scan).
        self._cancel.clear()
        # Always bust the target group's cache: the user asked to re-identify it, so its page
        # should reflect the outcome even if the attribution ends up unchanged.
        self._changed_rgs = {release_group_mbid}
        try:
            rows = await self._library.get_file_rows_for_album(release_group_mbid)
            by_folder: dict[str, list[Path]] = {}
            for row in rows:
                path = Path(row["file_path"])
                if path.exists():
                    by_folder.setdefault(str(path.parent), []).append(path)
            stats = ScanStats()
            for paths in by_folder.values():
                if self._cancel.is_set():
                    break
                entries: list[_FileEntry] = []
                for path in paths:
                    try:
                        tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
                        mtime = path.stat().st_mtime
                    except Exception as exc:  # noqa: BLE001 - a bad file must not abort the re-id
                        logger.warning("Re-identify: cannot read %s: %s", path, exc)
                        continue
                    entries.append(
                        _FileEntry(path=path, tag=self._enrich_tag_from_path(path, tag),
                                   info=info, mtime=mtime)
                    )
                if entries:
                    await self._identify_entries(entries, stats, force=True)
            # If re-identification moved every file off the old RG, drop its stale materialised
            # album row so it stops reporting "In Library" as a zero-file ghost.
            if not await self._library.has_album(release_group_mbid):
                await self._library.delete_album_row(release_group_mbid)
            logger.info(
                "reidentify.album",
                extra={"release_group_mbid": release_group_mbid, "matched": stats.matched},
            )
            return stats.matched
        finally:
            await self._flush_album_invalidations()

    async def startup_check(self, library_paths: list[Path]) -> None:
        """Resume an interrupted scan on boot (AUD-4): if state is ``scanning``,
        re-walk and skip the ledgered paths."""
        state = await self._state.get_state()
        if state.get("status") == "scanning":
            logger.info("Resuming interrupted library scan")
            await self.scan(library_paths, resume=True)

    async def scan(
        self, library_paths: list[Path], resume: bool = False, force: bool = False
    ) -> None:
        # Guard the singleton against overlapping scans (a manual start racing the
        # boot resume, or a double-start that slipped past the route's status check).
        if self._running:
            logger.warning(
                "A library scan is already running on this instance; ignoring overlapping start"
            )
            return
        self._running = True
        try:
            await self._run_scan(library_paths, resume, force)
        finally:
            self._running = False

    async def _run_scan(
        self, library_paths: list[Path], resume: bool = False, force: bool = False
    ) -> None:
        self._cancel.clear()
        self._changed_rgs = set()
        try:
            if resume:
                # os.walk is blocking; offload it.
                all_paths = await asyncio.to_thread(self._walk, library_paths)
                skip = await self._state.load_processed()
                # Seed matched/errored from the interrupted run so totals stay cumulative.
                prior = await self._state.get_state()
                stats = ScanStats(
                    matched=int(prior.get("matched_files") or 0),
                    errored=int(prior.get("failed_files") or 0),
                )
            else:
                # Mark 'scanning' before the walk so the UI flips immediately.
                await self._state.start(total_files=0)
                all_paths = await asyncio.to_thread(self._walk, library_paths)
                await self._state.set_total(len(all_paths))
                skip = set()
                stats = ScanStats()

            total = len(all_paths)
            # Force re-identifies everything: an empty index means no file matches
            # its unchanged signature, so the incremental skip never fires.
            file_index = {} if force else await self._library.get_file_index()
            processed = len(skip)
            batch: list[str] = []
            last_emit = time.monotonic()

            await self._events.publish(_SCAN_CHANNEL, "started", {"total": total})
            logger.info(
                "scan.started",
                extra={"total": total, "resume": resume, "force": force},
            )

            # Identify a folder at a time so a whole album's track list can be matched.
            folders = self._group_by_folder(all_paths)
            cancelled = False

            async def tick(spath: str) -> None:
                nonlocal processed, last_emit, batch
                processed += 1
                batch.append(spath)
                if len(batch) >= _LEDGER_BATCH_SIZE:
                    await self._flush(batch, processed, stats)
                    batch = []
                now = time.monotonic()
                if now - last_emit >= _PROGRESS_INTERVAL_SECONDS:
                    # Persist counters on the SSE cadence so polling never looks stuck.
                    await self._state.update_counters(
                        processed=processed, matched=stats.matched, failed=stats.errored
                    )
                    await self._emit_progress(processed, total, stats)
                    last_emit = now

            for folder_files in folders.values():
                if self._cancel.is_set():
                    cancelled = True
                    break
                todo = [p for p in folder_files if str(p) not in skip]
                if not todo:
                    continue

                if len(todo) > _MAX_ALBUM_FILES:
                    # Too big to be one album - identify per file, in ledger-sized
                    # chunks so tag reads parallelise and writes share a transaction.
                    for start in range(0, len(todo), _LEDGER_BATCH_SIZE):
                        if self._cancel.is_set():
                            cancelled = True
                            break
                        chunk = todo[start : start + _LEDGER_BATCH_SIZE]
                        prepared = await self._prepare_files(chunk, file_index, stats)
                        existing = await self._library.get_attributions_for_paths(
                            [str(e.path) for e in prepared if e is not None]
                        )
                        chunk_batch: list[dict] = []
                        done: list[str] = []
                        for path, entry in zip(chunk, prepared):
                            if self._cancel.is_set():
                                cancelled = True
                                break
                            if entry is not None:
                                await self._identify_and_persist(
                                    entry, stats, existing=existing, batch=chunk_batch
                                )
                            done.append(str(path))
                        await self._flush_persist_batch(chunk_batch)
                        for spath in done:
                            await tick(spath)
                    if cancelled:
                        break
                    continue

                entries: list[_FileEntry] = []
                for path, entry in zip(
                    todo, await self._prepare_files(todo, file_index, stats)
                ):
                    if entry is None:
                        await tick(str(path))
                    else:
                        entries.append(entry)
                # Ledger only the files actually persisted.
                for spath in await self._identify_entries(entries, stats):
                    await tick(spath)
                if self._cancel.is_set():
                    cancelled = True
                    break

            if cancelled:
                await self._state.cancel()
                await self._events.publish(
                    _SCAN_CHANNEL, "cancelled", {"stats": self._stats_dict(stats)}
                )
                return

            await self._flush(batch, processed, stats)
            await self._library.reconcile_with_filesystem(library_paths)
            await self._library.prune_review_for_imported()
            artists_resolved = await self._reconcile_album_artists()
            await self._state.complete(matched=stats.matched, failed=stats.errored)
            logger.info(
                "scan.completed",
                extra={
                    "processed": processed,
                    "total": total,
                    "matched": stats.matched,
                    "unmatched": stats.unmatched,
                    "errored": stats.errored,
                },
            )
            await self._emit_progress(processed, total, stats)
            complete_payload: dict = {"stats": self._stats_dict(stats)}
            if not artists_resolved:
                complete_payload["warning"] = (
                    "Some album artists couldn’t be resolved because MusicBrainz was "
                    "unreachable. Everything else imported fine - the artists will be "
                    "resolved automatically on the next scan."
                )
            await self._events.publish(_SCAN_CHANNEL, "complete", complete_payload)
        except Exception as exc:  # noqa: BLE001 - a scan fails closed, never crashes the loop
            logger.exception("scan.failed", extra={"error": str(exc)})
            await self._state.fail(str(exc))
            await self._events.publish(_SCAN_CHANNEL, "failed", {"error": str(exc)})
        finally:
            await self._flush_album_invalidations()

    async def _reconcile_album_artists(self) -> bool:
        """Give every matched album a canonical MusicBrainz artist, retrying transient failures. Returns False only when MusicBrainz stayed unreachable and some remain."""
        try:
            initial = await self._library.get_release_groups_needing_artist()
        except Exception as exc:  # noqa: BLE001 - never let this fail a scan
            logger.warning("Could not list release groups needing an artist: %s", exc)
            return True
        if not initial:
            return True

        total = len(initial)
        resolved: set[str] = set()
        await self._emit_finalizing(total, total)
        last_emit = time.monotonic()

        no_progress = 0
        for _attempt in range(_ARTIST_RECONCILE_PASSES):
            if self._cancel.is_set():
                return True
            try:
                pending = await self._library.get_release_groups_needing_artist()
            except Exception as exc:  # noqa: BLE001 - never let this fail a scan
                logger.warning("Could not list release groups needing an artist: %s", exc)
                return True
            if not pending:
                await self._emit_finalizing(0, total)
                return True
            progressed = False
            for rg in pending:
                if self._cancel.is_set():
                    return True
                try:
                    mbid, name = await self._album_identifier.resolve_release_group_artist(rg)
                    if mbid and name:
                        await self._library.set_album_artist(rg, mbid, name)
                        resolved.add(rg)
                        progressed = True
                except Exception as exc:  # noqa: BLE001 - one bad RG must not abort the rest
                    logger.warning("Artist reconcile failed for %s: %s", rg, exc)
                now = time.monotonic()
                if now - last_emit >= _PROGRESS_INTERVAL_SECONDS:
                    await self._emit_finalizing(max(0, total - len(resolved)), total)
                    last_emit = now
            if progressed:
                no_progress = 0
                continue
            no_progress += 1
            if no_progress >= 2:
                break
            logger.info(
                "Artist reconcile made no progress (MusicBrainz unreachable?); "
                "waiting %.0fs for the circuit breaker before retrying", _ARTIST_BREAKER_WAIT_S
            )
            await asyncio.sleep(_ARTIST_BREAKER_WAIT_S)
        try:
            remaining = await self._library.get_release_groups_needing_artist()
        except Exception:  # noqa: BLE001
            return True
        if remaining:
            logger.warning(
                "Artist reconcile left %d release group(s) unresolved (MusicBrainz "
                "unreachable); will retry on the next scan", len(remaining)
            )
        return not remaining

    async def _emit_finalizing(self, remaining: int, total: int) -> None:
        """Publish the post-files 'finalising - resolving artists' phase."""
        await self._events.publish(
            _SCAN_CHANNEL,
            "finalizing",
            {"phase": "artists", "remaining": remaining, "total": total},
        )

    @staticmethod
    def _group_by_folder(paths: list[Path]) -> dict[str, list[Path]]:
        """Group walked files by parent directory, preserving first-seen order."""
        groups: dict[str, list[Path]] = {}
        for path in paths:
            groups.setdefault(str(path.parent), []).append(path)
        return groups

    async def _prepare_file(
        self, path: Path, file_index: dict[str, tuple[float, int]], stats: ScanStats
    ) -> _FileEntry | None:
        """Stat, incremental-skip and tag read for one file; None if skipped or errored."""
        spath = str(path)
        try:
            stat = path.stat()
            signature = (stat.st_mtime, stat.st_size)
        except OSError as exc:
            logger.warning("Cannot stat %s: %s", path, exc)
            stats.errored += 1
            return None
        if file_index.get(spath) == signature:
            # Unchanged and already matched; count it so the tally reflects library state.
            stats.matched += 1
            return None

        try:
            tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
        except Exception as exc:  # noqa: BLE001 - a corrupt file must not kill the scan
            logger.warning("Cannot read tags from %s: %s", path, exc)
            stats.errored += 1
            return None

        tag = self._enrich_tag_from_path(path, tag)
        return _FileEntry(path=path, tag=tag, info=info, mtime=stat.st_mtime)

    async def _prepare_files(
        self,
        paths: list[Path],
        file_index: dict[str, tuple[float, int]],
        stats: ScanStats,
    ) -> list[_FileEntry | None]:
        """``_prepare_file`` for a folder's files with bounded parallelism - the
        stat + tag read are local-only work, so reading several files at once is
        safe; identification stays strictly sequential downstream. Results keep
        the input order (one slot per path, ``None`` where skipped/errored)."""
        if len(paths) == 1:
            return [await self._prepare_file(paths[0], file_index, stats)]
        semaphore = asyncio.Semaphore(_TAG_READ_CONCURRENCY)

        async def prepare(path: Path) -> _FileEntry | None:
            async with semaphore:
                return await self._prepare_file(path, file_index, stats)

        return list(await asyncio.gather(*(prepare(path) for path in paths)))

    async def _flush_persist_batch(self, batch: list[dict]) -> None:
        """Write a folder's buffered attributions in ONE transaction (see
        ``LibraryManager.upsert_files_batch``), then clear the buffer."""
        if batch:
            await self._library.upsert_files_batch(batch)
            batch.clear()

    async def _identify_entries(
        self, entries: list[_FileEntry], stats: ScanStats, *, force: bool = False
    ) -> list[str]:
        """Identify a folder's files as ONE album where possible, falling back to per-file;
        returns the paths persisted, in order.

        Two whole-folder attempts precede the per-file fallback so a folder's files never
        scatter across release groups: (1) a fast tag-based match, then (2) - only when the
        tags gave the matcher nothing - an AUDIO match that fingerprints the folder and
        feeds the matcher the resolved recordings + release groups, so junk tags (wrong
        album, compilation track numbers) can't win."""
        if not entries:
            return []
        persisted: list[str] = []
        claimed: set[str] = set()
        # Anchor (Phase 1): the folder's existing confident attributions seed the match
        # toward the album it already belongs to, and guard the persist so a re-scan can't
        # downgrade known-good identity. A forced re-identify (Phase 4 correction path)
        # skips both - an empty map means no seed bias and no persist guard.
        existing = (
            {}
            if force
            else await self._library.get_attributions_for_paths(
                [str(e.path) for e in entries]
            )
        )
        incumbent = self._incumbent_rg(existing)
        seeds = [incumbent] if incumbent else None
        # Skip the album lookup when every file is fully MBID-tagged or it's a single file.
        attempt = len(entries) >= 2 and any(
            not self._has_full_mbids(e.tag) for e in entries
        )
        if attempt:
            try:
                match = await self._album_identifier.identify(
                    [self._to_local_track(e) for e in entries],
                    seed_release_groups=seeds,
                )
            except Exception as exc:  # noqa: BLE001 - album match falls back to per-file
                logger.warning(
                    "Album identification failed for %s: %s", entries[0].path.parent, exc
                )
                match = None
            if match is not None and match.accepted:
                await self._claim_album_match(
                    entries, match, claimed, persisted, stats, existing
                )

        # Fingerprint-backed second attempt (the fix for scattered folders): only when the
        # tag match claimed nothing, so the extra AcoustID lookups stay bounded to the
        # folders that actually need them.
        fp_by_path: dict[str, "FingerprintResult"] = {}
        if attempt and not claimed and not self._cancel.is_set():
            try:
                enriched, seed_rgs, fp_by_path = await self._fingerprint_folder(entries)
                if incumbent and incumbent not in seed_rgs:
                    seed_rgs = [incumbent, *seed_rgs]
                match = (
                    await self._album_identifier.identify(
                        enriched, seed_release_groups=seed_rgs
                    )
                    if seed_rgs
                    else None
                )
                if match is not None and match.accepted:
                    await self._claim_album_match(
                        entries, match, claimed, persisted, stats, existing
                    )
                    logger.info(
                        "scan.album_fingerprint_matched",
                        extra={
                            "release_group_mbid": match.release_group_mbid,
                            "files": len(claimed),
                            "folder": str(entries[0].path.parent),
                        },
                    )
            except Exception as exc:  # noqa: BLE001 - the hard path must never kill a scan
                logger.warning(
                    "Fingerprint album match failed for %s: %s", entries[0].path.parent, exc
                )

        # Per-file fallback for anything not claimed as part of an album. Reuse any
        # fingerprint already taken above so Tier 3 doesn't fingerprint the file twice.
        batch: list[dict] = []
        for entry in entries:
            if str(entry.path) in claimed:
                continue
            if self._cancel.is_set():
                break
            await self._identify_and_persist(
                entry,
                stats,
                precomputed_fp=fp_by_path.get(str(entry.path)),
                existing=existing,
                batch=batch,
            )
            persisted.append(str(entry.path))
        # Flush before returning so the ledger never records an unwritten path.
        await self._flush_persist_batch(batch)
        return persisted

    def _incumbent_rg(self, existing: dict[str, dict]) -> str | None:
        """The folder's dominant *confident* prior release group (a download, or a
        high-confidence match) - the album it's presumed to already belong to. Seeds the
        matcher and biases the persist guard. ``None`` if the folder has no such anchor."""
        counts: Counter[str] = Counter()
        for row in existing.values():
            rg = row.get("release_group_mbid")
            if rg and (
                row.get("source") == "download"
                or float(row.get("confidence") or 0.0) >= _STICKY_CONFIDENCE
            ):
                counts[rg] += 1
        return counts.most_common(1)[0][0] if counts else None

    async def _is_studio_album_rg(self, release_group_mbid: str | None) -> bool | None:
        """True if the release group is a primary-type Album with no compilation/live
        secondary type; False if it's a compilation/live (or non-Album); ``None`` if the
        type can't be determined (MB miss - callers fail open)."""
        if not release_group_mbid:
            return None
        primary, secondary = await self._album_identifier.release_group_type(
            release_group_mbid
        )
        if primary is None and not secondary:
            return None
        return primary == "album" and not (secondary & _COMP_OR_LIVE)

    async def _should_keep_prior(
        self, prior: dict, new_rg: str | None, new_confidence: float
    ) -> bool:
        """Guard a scan re-attribution against downgrading known-good identity
        (ScannerAlbumIdentity Phase 1). Returns True to KEEP the prior attribution.

        - INV-1: never demote a studio album to a compilation/live release group. This
          invariant alone stops the observed Album->Compilation degradation.
        - Stickiness: a confident prior (a download, or a high-confidence match) is only
          overwritten by a clearly-better (higher-confidence) match."""
        prior_rg = prior.get("release_group_mbid")
        if not prior_rg or prior_rg == new_rg:
            return False  # no prior identity, or no change
        if await self._is_studio_album_rg(prior_rg) and (
            await self._is_studio_album_rg(new_rg) is False
        ):
            logger.info(
                "scan.attribution_kept_inv1",
                extra={"prior_rg": prior_rg, "new_rg": new_rg},
            )
            return True
        prior_conf = float(prior.get("confidence") or 0.0)
        anchored = prior.get("source") == "download" or prior_conf >= _STICKY_CONFIDENCE
        if anchored and new_confidence < prior_conf + _STICKY_MARGIN:
            logger.info(
                "scan.attribution_kept_sticky",
                extra={
                    "prior_rg": prior_rg,
                    "new_rg": new_rg,
                    "prior_confidence": prior_conf,
                    "new_confidence": new_confidence,
                },
            )
            return True
        return False

    async def _guarded_persist(
        self,
        entry: _FileEntry,
        existing: dict[str, dict],
        stats: ScanStats,
        *,
        release_group_mbid: str | None,
        release_mbid: str | None,
        recording_mbid: str | None,
        confidence: float,
        source: str,
        batch: list[dict] | None = None,
    ) -> None:
        """Persist a scan attribution for one file, unless a prior confident attribution
        should stand (Phase 1 guard). When the prior stands the row is left untouched -
        never downgraded - and still counted as matched. With ``batch`` the write is
        buffered for a single per-folder transaction (``_flush_persist_batch``)
        instead of committed per file."""
        prior = existing.get(str(entry.path))
        if prior is not None and await self._should_keep_prior(
            prior, release_group_mbid, confidence
        ):
            stats.matched += 1
            return
        if batch is not None:
            batch.append(
                {
                    "audio_path": entry.path,
                    "tag": entry.tag,
                    "info": entry.info,
                    "release_group_mbid": release_group_mbid,
                    "release_mbid": release_mbid,
                    "recording_mbid": recording_mbid,
                    "confidence": confidence,
                    "source": source,
                    "file_mtime": entry.mtime,
                }
            )
        else:
            await self._library.upsert_file(
                entry.path,
                entry.tag,
                entry.info,
                release_group_mbid=release_group_mbid,
                release_mbid=release_mbid,
                recording_mbid=recording_mbid,
                confidence=confidence,
                source=source,
                file_mtime=entry.mtime,
            )
        stats.matched += 1
        self._note_attribution_change(prior, release_group_mbid, release_mbid)

    def _note_attribution_change(
        self, prior: dict | None, new_rg: str | None, new_release: str | None
    ) -> None:
        """Flag the release groups whose cached album page is now stale, so the end-of-run
        invalidation busts only what actually moved. A new attribution, a moved release
        group, or a different release edition within the same group all count; re-writing
        the identical attribution does not (it would needlessly re-fetch from MusicBrainz)."""
        prior_rg = prior.get("release_group_mbid") if prior else None
        prior_release = prior.get("release_mbid") if prior else None
        if new_rg and (prior_rg != new_rg or prior_release != new_release):
            self._changed_rgs.add(new_rg)
        if prior_rg and prior_rg != new_rg:
            self._changed_rgs.add(prior_rg)

    async def _flush_album_invalidations(self) -> None:
        """Bust the cached pages of the release groups this run re-attributed, then clear the
        set. Never raises - a cache-invalidation failure must not fail the scan itself."""
        changed, self._changed_rgs = self._changed_rgs, set()
        if not changed or self._invalidate_albums is None:
            return
        try:
            await self._invalidate_albums(changed)
        except Exception as exc:  # noqa: BLE001 - invalidation is best-effort
            logger.warning("Album cache invalidation failed after scan: %s", exc)

    async def _claim_album_match(
        self,
        entries: list[_FileEntry],
        match: "AlbumMatch",
        claimed: set[str],
        persisted: list[str],
        stats: ScanStats,
        existing: dict[str, dict],
    ) -> None:
        """Persist EVERY file of an accepted whole-folder match under its ONE release group
        (Phase 2 - all-or-nothing): mapped files carry their track's recording at full
        confidence; the rest are kept as album members with a blank recording rather than
        scattered to a per-file guess. Already-claimed files, and files a prior confident
        attribution protects (Phase 1 guard), are left as-is."""
        mapped_confidence = round(1.0 - match.distance, 4)
        claimed_here = False
        batch: list[dict] = []
        for entry in entries:
            key = str(entry.path)
            if key in claimed:
                continue
            if self._cancel.is_set():
                break
            mapped = key in match.assignments
            await self._guarded_persist(
                entry,
                existing,
                stats,
                release_group_mbid=match.release_group_mbid,
                release_mbid=match.release_mbid,
                recording_mbid=(match.assignments.get(key) or None) if mapped else None,
                confidence=mapped_confidence if mapped else _UNMAPPED_ALBUM_CONFIDENCE,
                source="scan",
                batch=batch,
            )
            claimed.add(key)
            persisted.append(key)
            claimed_here = True
        # One transaction for the whole folder; must land before the artist stamp
        # below so set_album_artist sees the rows it updates.
        await self._flush_persist_batch(batch)
        # Stamp the canonical artist now so this album needs no end-of-scan lookup.
        if claimed_here and match.artist_mbid and match.artist_name:
            try:
                await self._library.set_album_artist(
                    match.release_group_mbid, match.artist_mbid, match.artist_name
                )
            except Exception as exc:  # noqa: BLE001 - the reconcile retries later
                logger.warning(
                    "Inline artist set failed for %s: %s", match.release_group_mbid, exc
                )

    async def _fingerprint_folder(
        self, entries: list[_FileEntry]
    ) -> tuple[list[LocalTrack], list[str], dict[str, "FingerprintResult"]]:
        """Fingerprint every file in a folder for the audio-based album match. Returns
        recording-enriched LocalTracks (an audio-confirmed recording MBID overrides the
        file's tag), the distinct release groups those recordings resolve to (the matcher's
        candidate seeds), and the raw fingerprints by path so the per-file fallback needn't
        fingerprint again. Fingerprinting fails open - a file we can't identify simply
        contributes its tag-only projection, so scatter degrades to today's behaviour, never
        worse."""
        enriched: list[LocalTrack] = []
        seed_rgs: list[str] = []
        seen: set[str] = set()
        fp_by_path: dict[str, "FingerprintResult"] = {}
        for entry in entries:
            local = self._to_local_track(entry)
            if self._cancel.is_set():
                enriched.append(local)
                continue
            # Fail open per file (honour the docstring): one file that can't fingerprint or
            # resolve must not abandon the whole folder's audio match - it just contributes
            # its tag-only projection, and the folder attempt (and fp reuse) survives.
            try:
                fp = await self._fingerprinter.fingerprint(entry.path)
                fp_by_path[str(entry.path)] = fp
                if (
                    fp.status == "pass"
                    and (fp.score or 0.0) >= _FINGERPRINT_SCORE_THRESHOLD
                    and fp.recording_id
                ):
                    local = msgspec.structs.replace(local, recording_mbid=fp.recording_id)
                    rg = await self._mb_matcher.resolve_recording_to_release_group(
                        fp.recording_id
                    )
                    if rg and rg not in seen:
                        seen.add(rg)
                        seed_rgs.append(rg)
            except Exception as exc:  # noqa: BLE001 - one bad file degrades to tag-only
                logger.warning("Fingerprint/resolve failed for %s: %s", entry.path, exc)
            enriched.append(local)
        return enriched, seed_rgs, fp_by_path

    # Maps the tiered identifier's verdict to a named log event. Per-file events
    # are DEBUG (a 10k scan emits one per file); lifecycle events stay INFO.
    _TIER_MATCH_EVENTS = {1: "scan.tier1_match", 2: "scan.tier2_match", 3: "scan.tier3_match"}

    async def _identify_and_persist(
        self,
        entry: _FileEntry,
        stats: ScanStats,
        precomputed_fp: "FingerprintResult | None" = None,
        existing: dict[str, dict] | None = None,
        batch: list[dict] | None = None,
    ) -> None:
        result = await self._identify_tiered(
            entry.path, entry.tag, entry.info, precomputed_fp=precomputed_fp
        )
        if result.matched:
            await self._guarded_persist(
                entry,
                existing or {},
                stats,
                release_group_mbid=result.release_group_mbid,
                release_mbid=result.release_mbid,
                recording_mbid=result.recording_mbid,
                confidence=result.confidence,
                source="scan",
                batch=batch,
            )
            logger.debug(
                self._TIER_MATCH_EVENTS.get(result.tier, "scan.matched"),
                extra={
                    "tier": result.tier,
                    "confidence": result.confidence,
                    "release_group_mbid": result.release_group_mbid,
                },
            )
        else:
            source = "acoustid" if result.fingerprint_attempted else "text_match"
            # The AcoustID recording id (when fingerprinting found one but the
            # release group couldn't be resolved) is the useful review candidate.
            candidates = [c for c in (result.recording_mbid, result.fingerprint) if c]
            await self._library.queue_for_manual_review(
                entry.path,
                entry.tag,
                entry.info,
                source=source,
                fingerprint=result.fingerprint,
                fingerprint_score=result.fingerprint_score,
                candidates=candidates,
            )
            stats.unmatched += 1
            logger.debug(
                "scan.tier4_unmatched",
                extra={"source": source, "fingerprint_score": result.fingerprint_score},
            )

    @staticmethod
    def _has_full_mbids(tag: "AudioTag") -> bool:
        return bool(tag.musicbrainz_release_group_id and tag.musicbrainz_recording_id)

    @staticmethod
    def _to_local_track(entry: _FileEntry) -> LocalTrack:
        tag, info = entry.tag, entry.info
        return LocalTrack(
            path=str(entry.path),
            title=tag.title or "",
            artist=tag.artist or tag.album_artist or "",
            album=tag.album or "",
            track_number=tag.track_number or 0,
            disc_number=tag.disc_number or 1,
            year=tag.year,
            duration_seconds=info.duration_seconds,
            recording_mbid=tag.musicbrainz_recording_id,
        )

    @staticmethod
    def _enrich_tag_from_path(path: Path, tag: "AudioTag") -> "AudioTag":
        """Fill missing artist/album/title/track/year from the file path; real tags win."""
        if tag.artist and tag.album and tag.title and tag.track_number:
            return tag
        parsed = parse_names_from_path(path)
        disc_number = tag.disc_number
        if disc_number == 1:
            m = re.match(r'^(?:cd|dis[ck])\s*0*(\d+)$', path.parent.name, re.IGNORECASE)
            if m:
                disc_number = int(m.group(1))
        return msgspec.structs.replace(
            tag,
            artist=tag.artist or parsed.artist or "",
            album=tag.album or parsed.album or "",
            title=tag.title or parsed.title or "",
            track_number=tag.track_number or parsed.track_number or 0,
            year=tag.year if tag.year is not None else parsed.year,
            disc_number=disc_number,
        )

    async def _identify_tiered(
        self,
        path: Path,
        tag: "AudioTag",
        info: "AudioInfo",
        precomputed_fp: "FingerprintResult | None" = None,
    ) -> TieredMatchResult:
        # Tier 1: MBIDs already in the file's tags.
        if tag.musicbrainz_release_group_id and tag.musicbrainz_recording_id:
            return TieredMatchResult(
                matched=True,
                tier=1,
                confidence=1.0,
                release_group_mbid=tag.musicbrainz_release_group_id,
                release_mbid=tag.musicbrainz_release_id,
                recording_mbid=tag.musicbrainz_recording_id,
            )

        # Tier 2: fuzzy text match against MusicBrainz (needs album + artist).
        if tag.album and tag.artist:
            try:
                mb = await self._mb_matcher.text_match(
                    TargetAlbum(
                        artist=tag.artist,
                        album=tag.album,
                        year=tag.year,
                        track_title=tag.title,
                        track_number=tag.track_number,
                        duration_seconds=info.duration_seconds,
                    )
                )
                if mb.matched:
                    return TieredMatchResult(
                        matched=True,
                        tier=2,
                        confidence=mb.confidence,
                        release_group_mbid=mb.release_group_mbid,
                        release_mbid=mb.release_mbid,
                        recording_mbid=mb.recording_mbid or mb.recording_mbids.get(tag.track_number),
                    )
            except Exception as exc:  # noqa: BLE001
                logger.warning("Text match failed for %s: %s", path, exc)

        # Tier 3: AcoustID fingerprint -> recording -> release group.
        fp_attempted = False
        fp_recording: str | None = None
        fp_score: float | None = None
        try:
            fp = (
                precomputed_fp
                if precomputed_fp is not None
                else await self._fingerprinter.fingerprint(path)
            )
        except Exception as exc:  # noqa: BLE001 - fingerprinting fails open
            logger.warning("Fingerprint failed for %s: %s", path, exc)
            fp = None
        # A real AcoustID verdict (pass/skip/fail) marks the file 'acoustid' for
        # manual review; 'disabled' (no key) and 'error' (fpcalc/HTTP broke) do not -
        # they shouldn't masquerade as "AcoustID found no match".
        if fp is not None and fp.status in ("pass", "skip", "fail"):
            fp_attempted = True
            fp_score = fp.score
        if (
            fp is not None
            and fp.status == "pass"
            and (fp.score or 0.0) >= _FINGERPRINT_SCORE_THRESHOLD
            and fp.recording_id
        ):
            fp_recording = fp.recording_id
            try:
                release_group = await self._mb_matcher.resolve_recording_to_release_group(
                    fp.recording_id
                )
            except Exception as exc:  # noqa: BLE001 - Tier 3 fails open to manual review
                logger.warning("Recording->release-group resolve failed for %s: %s", path, exc)
                release_group = None
            if release_group:
                return TieredMatchResult(
                    matched=True,
                    tier=3,
                    confidence=fp.score or 0.0,
                    release_group_mbid=release_group,
                    recording_mbid=fp.recording_id,
                    fingerprint_attempted=True,
                    fingerprint=fp.recording_id,
                    fingerprint_score=fp.score,
                )

        # Tier 4: manual review.
        return TieredMatchResult(
            matched=False,
            tier=4,
            fingerprint_attempted=fp_attempted,
            fingerprint=fp_recording,
            fingerprint_score=fp_score,
        )

    async def _flush(self, batch: list[str], processed: int, stats: ScanStats) -> None:
        if batch:
            await self._state.advance(
                batch, processed=processed, matched=stats.matched, failed=stats.errored
            )

    async def _emit_progress(self, processed: int, total: int, stats: ScanStats) -> None:
        await self._events.publish(
            _SCAN_CHANNEL,
            "progress",
            {
                "processed": processed,
                "total": total,
                "matched": stats.matched,
                "unmatched": stats.unmatched,
            },
        )

    @staticmethod
    def _stats_dict(stats: ScanStats) -> dict:
        return {
            "matched": stats.matched,
            "unmatched": stats.unmatched,
            "errored": stats.errored,
        }

    @staticmethod
    def _walk(library_paths: list[Path]) -> list[Path]:
        """Recursively collect audio files, skipping hidden directories. Uses the
        same suffix set as the reconcile walk so scan and soft-delete agree."""
        found: list[Path] = []
        for base in library_paths:
            for root, dirs, files in os.walk(base):
                dirs[:] = [d for d in dirs if not d.startswith(".")]
                for name in files:
                    if Path(name).suffix.lower() in _AUDIO_SUFFIXES:
                        found.append(Path(root) / name)
        return found
