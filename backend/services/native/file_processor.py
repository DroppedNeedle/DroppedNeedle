"""FileProcessor - verify + import downloaded files into the library.

Two halves:
- ``verify_downloaded_file``: confirm a file exists, its tags read, and key
  descriptive fields match. Used standalone for spot-checks.
- ``process_downloaded``: the import pipeline, per-file with continue-on-failure.
  For each expected file it resolves the on-disk source in slskd's download dir,
  verifies it, writes MBID tags, computes the target via the naming template,
  atomically moves it into the library, and inserts a ``library_files`` row. A bad
  file is recorded and skipped; the rest still import.

FileProcessor never touches slskd transfers - removing completed transfer records
after import is the orchestrator's job (via the client's ``cancel``).
"""

import asyncio
import errno
import logging
import os
import shutil
from pathlib import Path
from typing import TYPE_CHECKING
from uuid import uuid4

from infrastructure.msgspec_fastapi import AppStruct
from models.audio import AudioTag
from models.download_manifest import DownloadManifest, ExpectedFile

if TYPE_CHECKING:
    from infrastructure.audio.fingerprinter import AudioFingerprinter
    from infrastructure.audio.tagger import AudioTagger
    from repositories.protocols.download_client import DownloadClientProtocol
    from services.native.library_manager import LibraryManager
    from services.native.naming import NamingTemplateEngine

logger = logging.getLogger(__name__)

# failure reasons that mean "this source is bad" and warrant a quarantine row
# (matches the table CHECK). environment failures (a bad downloads mount) are not
# here - they must not blacklist an otherwise-good peer/file
QUARANTINE_REASONS = frozenset(
    {"verify_failed", "corrupt", "fingerprint_mismatch", "duration_mismatch"}
)
DOWNLOADS_MOUNT_UNAVAILABLE = (
    "downloads directory not accessible - check the slskd downloads mount"
)
# not a quarantine reason: the source file is fine, the failure is local
IMPORT_FAILED = "import failed - could not write the file into the library"
# not a quarantine reason: a per-track download whose duration doesn't match the
# requested recording is the WRONG track, not a bad file - fail over, don't blacklist
WRONG_TRACK = "wrong_track"
# not a quarantine reason: slskd reported the transfer finished but the file isn't
# where we look on the downloads mount (folder-name sanitisation, deeper nesting, or a
# downloads-mount path mismatch). The peer delivered fine; the fault is local, so
# failing over to another peer hits the same wall - never blacklist the source.
SOURCE_FILE_MISSING = "downloaded file not found on the downloads mount"


class VerifyStatus:
    PASS = "pass"
    FAIL = "fail"


class VerifyResult(AppStruct):
    status: str
    reason: str | None = None

    @property
    def passed(self) -> bool:
        return self.status == VerifyStatus.PASS


class VerificationFailed(Exception):
    """Per-file signal caught inside ``process_downloaded``'s loop, recorded as a
    ``FileFailure``, then the loop continues. Never HTTP-facing."""

    def __init__(
        self, message: str, *, reason: str = "verify_failed", filename: str | None = None
    ) -> None:
        super().__init__(message)
        self.reason = reason
        self.filename = filename


class AlreadyImported(Exception):
    """Crash-idempotency signal: a prior run already moved this file into the
    library (``os.replace`` succeeded, then we crashed before marking the task
    completed). Reconciled from the library and counted a success, not a failure."""

    def __init__(self, path: Path, *, filename: str | None = None) -> None:
        super().__init__(f"already imported: {filename}")
        self.path = path
        self.filename = filename


class FileFailure(AppStruct):
    """One failed file in a ``ProcessResult``."""

    filename: str
    reason: str


class ProcessResult(AppStruct):
    """Per-file outcome of ``process_downloaded`` (continue-on-failure)."""

    succeeded: list[str]
    failed: list[FileFailure]


def _matches(expected: str | None, actual: str | None) -> bool:
    if expected is None:
        return True
    return (actual or "").strip().casefold() == expected.strip().casefold()


def _basename(filename: str) -> str:
    """Last path segment (slskd filenames use backslashes); log basenames not full
    peer paths to keep log lines free of identifying directory structure."""
    return filename.replace("\\", "/").rsplit("/", 1)[-1]


class FileProcessor:
    def __init__(
        self,
        tagger: "AudioTagger",
        *,
        naming_engine: "NamingTemplateEngine | None" = None,
        library_manager: "LibraryManager | None" = None,
        library_paths: list[Path] | None = None,
        client: "DownloadClientProtocol | None" = None,
        slskd_downloads_path: Path | None = None,
        fingerprinter: "AudioFingerprinter | None" = None,
        verify_downloads: bool = True,
    ) -> None:
        self._tagger = tagger
        self._naming = naming_engine
        self._library = library_manager
        self._library_paths = library_paths or []
        self._client = client
        self._slskd_downloads_path = (
            Path(slskd_downloads_path) if slskd_downloads_path else None
        )
        self._fingerprinter = fingerprinter
        self._verify_downloads = verify_downloads

    def verify_downloaded_file(
        self,
        path: Path,
        *,
        title: str | None = None,
        artist: str | None = None,
        album: str | None = None,
        track_number: int | None = None,
    ) -> VerifyResult:
        """PASS when the file exists, its tags read, and every supplied expected
        field matches; FAIL (with a reason) otherwise. Supplying no expectations
        verifies readability only."""
        if not path.exists():
            return VerifyResult(status=VerifyStatus.FAIL, reason="file not found")
        try:
            tag, _info = self._tagger.read_tags(path)
        except Exception as exc:  # noqa: BLE001 - any read failure is a verify failure
            logger.warning("verify_downloaded_file: unreadable %s: %s", path, exc)
            return VerifyResult(status=VerifyStatus.FAIL, reason="tags unreadable")
        if not _matches(title, tag.title):
            return VerifyResult(status=VerifyStatus.FAIL, reason="title mismatch")
        if not _matches(artist, tag.artist):
            return VerifyResult(status=VerifyStatus.FAIL, reason="artist mismatch")
        if not _matches(album, tag.album):
            return VerifyResult(status=VerifyStatus.FAIL, reason="album mismatch")
        if track_number is not None and tag.track_number != track_number:
            return VerifyResult(status=VerifyStatus.FAIL, reason="track number mismatch")
        return VerifyResult(status=VerifyStatus.PASS)

    async def process_downloaded(
        self,
        manifest: DownloadManifest,
        only_filenames: set[str] | None = None,
    ) -> ProcessResult:
        """Import each expected file from slskd's download dir into the library.

        Continue-on-failure: a bad file is recorded and skipped, the rest still
        import. The orchestrator quarantines each failure and derives
        completed/partial/failed from the counts (this is what makes ``partial``
        reachable).

        ``only_filenames`` restricts the import to a subset of the manifest - the
        orchestrator passes the files whose slskd transfer actually succeeded, so a
        stalled task imports what arrived without the never-arrived files being
        recorded as (quarantinable) verification failures."""
        if self._naming is None or self._library is None or not self._library_paths \
                or self._client is None:
            # in production all four are injected by the DI provider
            raise RuntimeError("FileProcessor is not configured for downloads")

        succeeded: list[str] = []
        failed: list[FileFailure] = []
        targets = manifest.target_files
        if only_filenames is not None:
            targets = [f for f in targets if f.filename in only_filenames]
        for expected in targets:
            try:
                target = await self._process_one(expected, manifest)
                succeeded.append(str(target))
            except AlreadyImported as already:
                # prior run already imported this file: count its library path a
                # success, do not quarantine
                succeeded.append(str(already.path))
            except VerificationFailed as failure:
                failed.append(
                    FileFailure(
                        filename=failure.filename or expected.filename,
                        reason=failure.reason,
                    )
                )
                logger.info(
                    "process.verify_failed",
                    extra={
                        "task_id": manifest.task_id,
                        "file": _basename(failure.filename or expected.filename),
                        "reason": failure.reason,
                    },
                )

        if succeeded:
            # targeted reconcile so new files surface immediately and stale rows in
            # the album dir are cleaned; best-effort, never fails the import
            parents = list({Path(p).parent for p in succeeded})
            try:
                await self._library.reconcile_with_filesystem(targets=parents)
            except Exception:  # noqa: BLE001 - reconcile is best-effort
                logger.warning("post-import reconcile failed for task %s", manifest.task_id)

        logger.info(
            "process.completed",
            extra={
                "task_id": manifest.task_id,
                "succeeded": len(succeeded),
                "failed": len(failed),
            },
        )
        return ProcessResult(succeeded=succeeded, failed=failed)

    async def _process_one(
        self, expected: ExpectedFile, manifest: DownloadManifest
    ) -> Path:
        """Verify -> tag -> move -> insert one file. Raises ``VerificationFailed``
        (per-file) or ``AlreadyImported`` (crash-idempotency)."""
        source = await self._client.get_file_path(
            manifest.source_username, expected.filename, expected.size
        )

        # distinguish a bad downloads mount (environment fault) from a single missing
        # file: a bad mount fails this file with a sanitized reason but never
        # quarantines (not the source's fault)
        downloads_root = self._slskd_downloads_path
        if downloads_root is None or not downloads_root.is_dir() \
                or not os.access(downloads_root, os.R_OK):
            raise VerificationFailed(
                f"Downloads mount unavailable for {expected.filename}",
                reason=DOWNLOADS_MOUNT_UNAVAILABLE,
                filename=expected.filename,
            )

        if source is None or not source.exists() or not os.access(source, os.R_OK):
            # (a) parent dir unreadable -> mount went away under us
            parent_ok = (
                source is not None
                and source.parent.is_dir()
                and os.access(source.parent, os.R_OK)
            )
            if source is not None and not parent_ok:
                raise VerificationFailed(
                    f"Downloads mount unavailable for {expected.filename}",
                    reason=DOWNLOADS_MOUNT_UNAVAILABLE,
                    filename=expected.filename,
                )
            # (b) mount healthy but source gone: signature of a prior-run import;
            # reconcile from the library, do not fail/quarantine
            existing = await self._library.get_imported_file(
                download_task_id=manifest.task_id, filename=expected.filename
            )
            # only reconcile as already-imported if that library file is still on disk;
            # a row whose file was deleted out-of-band isn't a real success, so fall
            # through to (c) rather than falsely counting it imported
            if existing is not None and Path(existing["file_path"]).exists():
                logger.info(
                    "File %s already imported on a prior run (task %s); reconciling "
                    "from the library instead of failing",
                    expected.filename,
                    manifest.task_id,
                )
                raise AlreadyImported(
                    Path(existing["file_path"]), filename=expected.filename
                )
            # (c) mount healthy but the file isn't where we look -> a local locate
            # failure, not the peer's fault (SOURCE_FILE_MISSING is non-quarantine)
            raise VerificationFailed(
                f"Missing file: {expected.filename}",
                reason=SOURCE_FILE_MISSING,
                filename=expected.filename,
            )

        # mutagen is sync; wrap in to_thread, mirroring the scanner
        try:
            tag, info = await asyncio.to_thread(self._tagger.read_tags, source)
        except Exception as exc:  # noqa: BLE001 - any read failure is a corrupt file
            raise VerificationFailed(
                f"Cannot read tags: {exc}", reason="corrupt", filename=expected.filename
            ) from exc

        target_tag = self._build_target_tag(manifest, tag)
        target_path = self._library_paths[0] / self._naming.format_path(
            manifest.naming_template, target_tag, info.file_format
        )

        # Position-level dedup, before the expensive verification below: if the album
        # already holds a file at this (disc, track), don't write a second copy. Stops
        # the flac-vs-mp3 / failover re-pull duplicate (completeness dedupes by position,
        # but the files themselves never were) and skips fingerprinting a copy we
        # discard. Known track numbers only; an untagged file (track 0) falls through to
        # the path check below.
        if target_tag.track_number:
            present = await self._library.get_file_at_position(
                manifest.release_group_mbid,
                target_tag.disc_number or 1,
                target_tag.track_number,
            )
            if present is not None and present.get("file_path") != str(target_path):
                logger.info(
                    "process.duplicate_position",
                    extra={
                        "task_id": manifest.task_id,
                        "file": _basename(expected.filename),
                        "disc": target_tag.disc_number or 1,
                        "track": target_tag.track_number,
                    },
                )
                # the track is already in the library from another source - drop the
                # redundant slskd copy and keep the existing import (counted a success)
                try:
                    source.unlink()
                except OSError:
                    logger.warning("Could not remove duplicate source %s", source)
                self._prune_empty_source_dirs(source)
                return Path(present["file_path"])

        # duration sanity, always on: catches "right filename, wrong audio".
        # Tolerance is the larger of 15s or 10% of the expected length, so normal
        # master/encoder variance passes. For a per-track download the expected value
        # is the CANONICAL track length, so a mismatch means the wrong recording was
        # picked - fail over (WRONG_TRACK), don't quarantine an otherwise-good file.
        if expected.duration and info.duration_seconds and abs(
            info.duration_seconds - expected.duration
        ) > max(15.0, 0.10 * expected.duration):
            raise VerificationFailed(
                f"Duration mismatch ({info.duration_seconds:.0f}s vs "
                f"{expected.duration:.0f}s)",
                reason=WRONG_TRACK if manifest.is_track else "duration_mismatch",
                filename=expected.filename,
            )
        # AcoustID release-group check, only when verify is on and a fingerprinter is
        # wired. fail-open: the fingerprinter never raises, a non-pass/empty result
        # skips, and only a confident different release group is rejected
        if self._verify_downloads and self._fingerprinter is not None:
            fp = await self._fingerprinter.fingerprint(source)
            if (
                fp.status == "pass"
                and fp.release_group_ids
                and manifest.release_group_mbid not in fp.release_group_ids
            ):
                raise VerificationFailed(
                    "AcoustID identified a different release group",
                    reason="fingerprint_mismatch",
                    filename=expected.filename,
                )

        # mutating phase (stage -> tag -> publish -> insert): an I/O or DB error must
        # fail just this file, not abort the album and orphan files imported earlier
        try:
            target_path.parent.mkdir(parents=True, exist_ok=True)

            if target_path.exists():
                # already imported on a prior run; drop the leftover slskd source
                logger.info(
                    "Target %s already present; skipping import (already imported)", target_path
                )
                try:
                    source.unlink()
                except OSError:
                    logger.warning("Could not remove leftover source %s", source)
                self._prune_empty_source_dirs(source)
            else:
                # copy/tag/rename are blocking I/O -> run off the event loop
                await asyncio.to_thread(
                    self._import_into_library, source, target_path, target_tag
                )
                logger.info(
                    "process.file_tagged",
                    extra={
                        "task_id": manifest.task_id,
                        "file": _basename(expected.filename),
                    },
                )
                logger.info(
                    "process.file_moved",
                    extra={
                        "task_id": manifest.task_id,
                        "file": _basename(expected.filename),
                        "target": target_path.name,
                    },
                )

            await self._library.upsert_file(
                target_path,
                target_tag,
                info,
                release_group_mbid=manifest.release_group_mbid,
                release_mbid=manifest.release_mbid,
                recording_mbid=tag.musicbrainz_recording_id,
                confidence=1.0,
                source="download",
                download_task_id=manifest.task_id,
                source_path=str(source),
            )
        except Exception as exc:  # noqa: BLE001 - import I/O or DB error -> per-file failure
            raise VerificationFailed(
                f"Import failed for {expected.filename}: {exc}",
                reason=IMPORT_FAILED,
                filename=expected.filename,
            ) from exc
        return target_path

    def _import_into_library(
        self, source: Path, target_path: Path, target_tag: AudioTag
    ) -> None:
        """Bring a finished download onto the library side and publish it atomically.

        slskd's download dir is often a separate mount from the library (Docker
        volumes, even on one filesystem), where a direct ``rename`` across mounts
        fails with ``EXDEV``. So we stage into the destination dir on the library's
        mount: ``rename`` the source in when it shares the mount, else copy. Tags are
        written on our staged copy, never on slskd's file in place, and the final
        placement is always an ``os.replace`` within one directory. After a
        cross-mount copy the slskd source is removed so there's no doubled storage."""
        tmp = target_path.parent / f".{target_path.stem}.{uuid4().hex[:8]}.part"
        consumed_source = False
        try:
            try:
                os.replace(source, tmp)  # same mount: atomic, no copy
                consumed_source = True
            except OSError as exc:
                if exc.errno != errno.EXDEV:
                    raise
                try:
                    shutil.copy2(source, tmp)  # cross-mount: the one unavoidable copy
                except BaseException:
                    shutil.copyfile(source, tmp)  # Full fallback, just copy data
            self._tagger.write_album_identity(tmp, target_tag)
            os.replace(tmp, target_path)  # atomic publish within the library dir
        except BaseException:
            # same-mount path renamed source into tmp; if tagging/publish then fails,
            # restore slskd's original so a failed import never destroys the only copy
            if consumed_source:
                try:
                    os.replace(tmp, source)
                except OSError:
                    pass
                else:
                    raise
            try:
                tmp.unlink(missing_ok=True)
            except OSError:
                pass
            raise
        if not consumed_source:
            try:
                source.unlink()
            except OSError:
                logger.warning("Imported but could not remove slskd source %s", source)
        self._prune_empty_source_dirs(source)

    def _prune_empty_source_dirs(self, source: Path) -> None:
        """Remove the now-empty folders slskd left behind after a file is moved out of
        the downloads dir, walking up to but not including the mount root. ``rmdir``
        only deletes empty dirs, so a half-imported album (siblings still present) is
        left alone. Best-effort: never fails the import."""
        mount = self._slskd_downloads_path
        if mount is None:
            return
        try:
            mount = mount.resolve()
            directory = source.parent.resolve()
        except OSError:
            return
        while directory != mount and mount in directory.parents:
            try:
                directory.rmdir()  # only succeeds when empty
            except OSError:
                return  # non-empty (other tracks pending) or already gone -> stop
            directory = directory.parent

    @staticmethod
    def _build_target_tag(manifest: DownloadManifest, file_tag: AudioTag) -> AudioTag:
        """Merge the file's existing descriptive tags with the manifest's album
        identity (the authoritative MBIDs come from the request, not the audio)."""
        return AudioTag(
            title=file_tag.title,
            artist=file_tag.artist,
            album=manifest.album_title,
            album_artist=manifest.artist_name,
            track_number=file_tag.track_number,
            disc_number=file_tag.disc_number or 1,
            year=manifest.year,
            genre=file_tag.genre,
            musicbrainz_release_group_id=manifest.release_group_mbid,
            musicbrainz_release_id=manifest.release_mbid,
            musicbrainz_recording_id=file_tag.musicbrainz_recording_id,
            musicbrainz_artist_id=file_tag.musicbrainz_artist_id,
            musicbrainz_album_artist_id=manifest.artist_mbid,
            acoustid_id=file_tag.acoustid_id,
            compilation=file_tag.compilation,
        )
