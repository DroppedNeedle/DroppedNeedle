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
import re
import shutil
from pathlib import Path
from typing import TYPE_CHECKING, NamedTuple
from uuid import uuid4

from rapidfuzz import fuzz

from infrastructure.msgspec_fastapi import AppStruct
from models.audio import AudioInfo, AudioTag
from infrastructure.audio.fingerprinter import split_artist_credit
from models.download_manifest import DownloadManifest, ExpectedFile, ExpectedTrack
from services.native.quality_tiers import tier_for, tier_rank
from services.native.recycle_bin import recycle
from services.native.title_match import names_different_album, title_containment_score

if TYPE_CHECKING:
    from infrastructure.audio.fingerprinter import AudioFingerprinter
    from infrastructure.audio.tagger import AudioTagger
    from infrastructure.persistence.download_store import DownloadStore
    from models.held_import import HeldImport
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


# Folder-import matcher (D18). Matches enumerated files to the MB tracklist by duration
# (the always-available signal), with tags + filename as tie-breakers.
_TRACK_NUM_RE = re.compile(r"^\s*(\d{1,3})\s*[\.\-_)]+\s*")  # "01. ", "1 - ", "03_ "
_DISC_DIR_RE = re.compile(r"\b(?:cd|disc|disk)\s*(\d+)\b", re.IGNORECASE)


class _FolderCandidate(NamedTuple):
    path: Path
    tag: AudioTag
    info: AudioInfo


def _filename_track_number(path: Path) -> int | None:
    match = _TRACK_NUM_RE.match(path.name)
    return int(match.group(1)) if match else None


def _filename_title(path: Path) -> str:
    """The filename with a leading track number + the extension stripped, e.g.
    ``02. Fearless.flac`` → ``Fearless``. A tie-breaker only (never a gate), so an
    obfuscated name simply doesn't match anything."""
    stem = path.stem
    return _TRACK_NUM_RE.sub("", stem).strip()


def _candidate_disc(candidate: _FolderCandidate) -> int:
    if candidate.tag.disc_number:
        return candidate.tag.disc_number
    # CD1/Disc 2 path heuristic for multi-disc rips that lack disc tags (review C).
    match = _DISC_DIR_RE.search(candidate.path.parent.name)
    return int(match.group(1)) if match else 1


_TITLE_CONFLICT_RATIO = 50  # below this, a real title tag names a different track


def _title_conflicts(candidate: _FolderCandidate, track: ExpectedTrack) -> bool:
    """True when the file carries a real TITLE tag that clearly does NOT name the matched
    MB track - i.e. a duration coincidence on the wrong recording (e.g. a live track whose
    length happens to match a studio track). Lidarr matches by metadata, not duration
    alone. Untagged rips (no title tag) fall through to duration matching, so this never
    blocks a genuinely-untagged correct file (the D18 case)."""
    if not track.title:
        return False
    tag_title = (candidate.tag.title or "").strip()
    if not tag_title:
        return False  # untagged -> trust duration/filename, don't reject on title
    return fuzz.token_set_ratio(tag_title, track.title) < _TITLE_CONFLICT_RATIO


def _import_confidence(*, tag, info, expected_track, canonical_duration, fp) -> float:  # noqa: ANN001
    """Attribution confidence for a download import, on the SCANNER's existing scale
    (P7/D7 - the hardcoded 1.0 meant "we trust the album identity", not "this audio is
    the requested recording", and froze wrong imports at fake-perfect forever):

    - **1.0** - recording identity positively confirmed: the file's own recording-MBID
      tag, or a confident AcoustID hit, names the expected recording.
    - **0.9** - the canonical MusicBrainz length validated the audio (the strict
      duration gate passed).
    - **0.8** - title/tag agreement only (containment-strong title, no measurable
      duration) - below the scanner's 0.85 sticky line, so a better scan match may
      later re-identify it (``source='download'`` still anchors it meanwhile).
    - **0.6** - imported with no identity corroboration (a multi-file album import
      that merely didn't conflict).

    The held-import "import anyway" path deliberately stays 1.0 - a human attribution
    decision outranks every heuristic (D8)."""
    expected_rec = (
        (expected_track.recording_mbid or "").strip().lower() if expected_track else ""
    )
    if expected_rec:
        tag_rec = (tag.musicbrainz_recording_id or "").strip().lower()
        if tag_rec and tag_rec == expected_rec:
            return 1.0
        fp_rec = (getattr(fp, "recording_id", None) or "").strip().lower() if fp else ""
        if fp_rec and getattr(fp, "status", None) == "pass" and fp_rec == expected_rec:
            return 1.0
    if canonical_duration and info.duration_seconds and abs(
        info.duration_seconds - canonical_duration
    ) <= max(15.0, 0.10 * canonical_duration):
        return 0.9
    expected_title = expected_track.title if expected_track else None
    tag_title = (tag.title or "").strip()
    if expected_title and tag_title and (
        title_containment_score(expected_title, tag_title) >= _TAG_TITLE_WEAK
    ):
        return 0.8
    return 0.6


def row_covers_track(
    row: dict,
    *,
    recording_mbid: str | None,
    title: str | None,
    duration_seconds: float | None,
) -> bool:
    """Whether a ``library_files`` row is plausibly THE expected track (P4 coverage,
    shared by the completeness gate and the position-dedup guard): recording MBID
    match, else duration within the standard tolerance, else a containment-strong
    title. An untagged row with no measurable signal counts as covering (D18
    fail-open - absence of evidence is unknown, not a mismatch); a row that
    POSITIVELY disagrees on every known signal does not."""
    row_rec = (row.get("recording_mbid") or "").strip().lower()
    if recording_mbid and row_rec:
        return row_rec == recording_mbid.strip().lower()
    row_dur = row.get("duration_seconds")
    if duration_seconds and row_dur:
        return abs(row_dur - duration_seconds) <= max(15.0, 0.10 * duration_seconds)
    row_title = (row.get("track_title") or "").strip()
    if title and row_title:
        return title_containment_score(title, row_title) >= _TAG_TITLE_WEAK
    return True


_TAG_ARTIST_CONFLICT_RATIO = 55  # below: the artist TAG names someone else. token_set is
# subset-tolerant, so feat. credits score 100 and never conflict; classical
# composer-vs-performer pairs DO conflict, which is why the hold needs a second signal.
_TAG_TITLE_WEAK = 0.60  # containment below: the title tag doesn't name the expected track


def _tag_conflict_reason(tag, info, manifest, expected_track) -> str | None:  # noqa: ANN001
    """``"tag_mismatch"`` when the file's OWN embedded tags clearly name different
    content than the request, else ``None`` (P2, 2026-07-05 wrong-single incident -
    the file was tagged "Dan Romer / Arrival in Ashford / track 2" and imported anyway).

    Works when AcoustID has no coverage (its exact fail-open hole). Fail-open rules
    mirror the folder path's D18: untagged fields never conflict, various-artists
    albums skip the artist check, and an artist conflict ALONE never holds - classical
    tags carry the composer while the album artist is the performer, so a hold needs a
    weak/conflicting title or a conflicting duration on top."""
    expected_artist = manifest.artist_name or ""
    tag_artist = (tag.artist or "").strip()
    artist_conflict = bool(
        tag_artist
        and expected_artist
        and "various" not in expected_artist.lower()
        and fuzz.token_set_ratio(tag_artist, expected_artist) < _TAG_ARTIST_CONFLICT_RATIO
    )
    tag_title = (tag.title or "").strip()

    if expected_track is not None and expected_track.title:
        # A real title tag naming a clearly different SONG rejects on its own
        # (same rule as the folder path's _title_conflicts).
        if tag_title and fuzz.token_set_ratio(tag_title, expected_track.title) < _TITLE_CONFLICT_RATIO:
            return "tag_mismatch"
        if artist_conflict:
            title_weak = bool(tag_title) and (
                title_containment_score(expected_track.title, tag_title) < _TAG_TITLE_WEAK
            )
            expected_dur = expected_track.duration_seconds
            duration_conflict = bool(
                expected_dur
                and info.duration_seconds
                and abs(info.duration_seconds - expected_dur) > max(15.0, 0.10 * expected_dur)
            )
            if title_weak or duration_conflict:
                return "tag_mismatch"
        return None

    # No expected track (a multi-file album import, or identity threading failed):
    # the ALBUM tag is the only per-file cross-check - hold only on the pair.
    if artist_conflict:
        tag_album = (tag.album or "").strip()
        if tag_album and title_containment_score(manifest.album_title or "", tag_album) < _TAG_TITLE_WEAK:
            return "tag_mismatch"
    return None


def _fingerprint_disagrees(fp, expected_track, expected_artist: str | None) -> bool:
    """True only when AcoustID CONFIDENTLY (status=pass) identified the audio as a clearly
    different SONG, or a clearly different ARTIST, than expected. Release-group/edition is
    deliberately NOT checked: AcoustID's RG coverage is incomplete and one recording appears
    on many editions (original / reissue / compilation), so gating on the requested RG
    false-rejects valid tracks (e.g. a 2011 reissue + its BBC-session bonuses). Lidarr
    verifies recording identity, not edition, and fails OPEN - a non-pass result never
    rejects, leaving the tag/duration match to stand. ``expected_track`` may be None (the
    slskd path has no per-file title), in which case only the artist is checked."""
    if getattr(fp, "status", None) != "pass":
        return False
    fp_title = (getattr(fp, "title", None) or "").strip()
    fp_artist = (getattr(fp, "artist", None) or "").strip()
    expected_title = getattr(expected_track, "title", None) if expected_track is not None else None
    if fp_title and expected_title and fuzz.token_set_ratio(fp_title, expected_title) < 50:
        return True  # clearly the wrong song
    # Wrong artist - but skip for various-artists compilations, where the album artist
    # legitimately differs from a track's performing artist.
    if fp_artist and expected_artist and "various" not in expected_artist.lower():
        # A collab credit ("Artist A; Artist B") is NOT a wrong artist when the
        # requested artist is any credited member - compare every credit token on
        # both sides plus the full strings (token_set_ratio alone scores "Artist A"
        # vs "Artist A; Artist B" under the gate: the separator glues tokens).
        # This wires split_artist_credit into the verify path, per its plan note.
        fp_credits = split_artist_credit(fp_artist) + [fp_artist]
        expected_credits = split_artist_credit(expected_artist) + [expected_artist]
        best = max(
            fuzz.token_set_ratio(fp_credit, expected_credit)
            for fp_credit in fp_credits
            for expected_credit in expected_credits
        )
        if best < 55:
            # Remix: AcoustID credits the ORIGINAL artist - "Song (Artist B
            # remix)" carries the credit of "Song"'s artist. The requested artist
            # named in the track title is positive evidence of the right
            # recording, not a wrong-artist hit.
            titles = f"{expected_title or ''} {fp_title or ''}".lower()
            if expected_artist.lower() not in titles:
                return True
    return False


def _pair_score(
    candidate: _FolderCandidate, track: ExpectedTrack, *, sole_track: bool = False
) -> float | None:
    """Score a (file, MB-track) pair, or ``None`` if they can't be the same track.
    **Duration gates** the match and a conflicting title tag vetoes it; tags + filename
    break ties (so a rip with shifted track numbers - e.g. merged tracks - still matches
    the right songs by duration + title, not by the misleading position).

    ``sole_track``: the release expects exactly ONE track (a single). When its MB
    length is also unknown - the common state for brand-new releases - the duration
    gate is disarmed and "track 1 of the wrong album" would match on position alone
    with only the loose <50 title veto (token_set scores "Arrival" a perfect 100
    against "the arrival"). A tagged title must then positively NAME the expected
    track (containment), not merely fail to conflict (2026-07-05 incident, P2.5).
    Untagged files still fall through to the position match (D18)."""
    file_dur = candidate.info.duration_seconds
    track_dur = track.duration_seconds
    disc_ok = _candidate_disc(candidate) == (track.disc_number or 1)
    score = 0.0

    if track_dur and file_dur:
        tolerance = max(15.0, 0.10 * track_dur)
        if abs(file_dur - track_dur) > tolerance:
            return None  # duration gate
        score += max(0.0, 100.0 - abs(file_dur - track_dur))
    else:
        # No duration to compare on one/both sides → fall back to a track-number match.
        candidate_track = candidate.tag.track_number or _filename_track_number(candidate.path)
        if candidate_track != track.track_number or not disc_ok:
            return None
        tag_title = (candidate.tag.title or "").strip()
        if sole_track and track.title and tag_title and (
            title_containment_score(track.title, tag_title) < _TAG_TITLE_WEAK
        ):
            return None  # positional coincidence: the tag names other content
        score += 50.0

    # Title corroboration (Lidarr matches by metadata, not duration alone): a real title tag
    # naming a DIFFERENT track means the duration match is a coincidence - veto it, unless
    # the recording MBID confirms identity.
    mbid_match = bool(
        track.recording_mbid and candidate.tag.musicbrainz_recording_id == track.recording_mbid
    )
    if not mbid_match and _title_conflicts(candidate, track):
        return None

    # Tie-breakers (decisive → weak). Title/identity signals are deliberately ranked ABOVE
    # the track-number signals: a rip can carry shifted/wrong track numbers (e.g. merged
    # tracks) while the title is correct, so a misleading position must never override a
    # title match when two adjacent tracks have similar durations.
    if track.recording_mbid and candidate.tag.musicbrainz_recording_id == track.recording_mbid:
        score += 1000.0
    if track.title:
        if candidate.tag.title and candidate.tag.title.strip().casefold() == track.title.strip().casefold():
            score += 40.0  # an exact title tag is strong identity
        else:
            ratio = fuzz.token_set_ratio(_filename_title(candidate.path), track.title)
            if ratio >= 80:
                score += 30.0 * ratio / 100.0  # filename title is the next-best identity
    if candidate.tag.track_number == track.track_number and disc_ok:
        score += 15.0  # weaker than title - positions are unreliable on real rips
    if _filename_track_number(candidate.path) == track.track_number:
        score += 10.0
    return score


def _folder_names_wrong_album(
    candidates: list[_FolderCandidate], target_artist: str, target_album: str
) -> bool:
    """True when a confident MAJORITY of the enumerated files' ALBUM tags name a DIFFERENT
    album than the requested one - the IMPORT-time half of the wrong-album guard (#1). A Usenet
    release whose obfuscated NAME slipped the grab-time guard is caught here once unpacked (the
    rip's one reliable tag is usually ``album``). Reuses ``names_different_album`` (edition-
    tolerant, same-artist-aware) on a reconstructed ``"artist album"`` so its artist-gating
    applies. TAGLESS files (no album tag) are EXCLUDED, so an all-untagged rip never trips this
    (it falls through to duration matching), and a partial-but-correct album whose tags match is
    kept - only a wholly WRONG album is rejected (owner Q1: partials are never discarded)."""
    tagged = [c for c in candidates if (c.tag.album or "").strip()]
    if not tagged:
        return False

    def _wrong(c: _FolderCandidate) -> bool:
        artist = (c.tag.album_artist or c.tag.artist or target_artist or "").strip()
        return names_different_album(target_album, target_artist, f"{artist} {c.tag.album}")

    return sum(1 for c in tagged if _wrong(c)) * 2 > len(tagged)


def _match_folder_to_tracklist(
    candidates: list[_FolderCandidate], expected: list[ExpectedTrack]
) -> list[tuple[_FolderCandidate, ExpectedTrack]]:
    """Greedy 1:1 assignment of files to tracklist positions by descending pair score.
    Each file imports at most once; each MB position is filled at most once. Files that
    match no position (bonus tracks, samples, a merged-track file) are simply not
    returned → dropped (owner Q1)."""
    scored: list[tuple[float, int, int]] = []
    sole_track = len(expected) == 1
    for ci, candidate in enumerate(candidates):
        for ti, track in enumerate(expected):
            score = _pair_score(candidate, track, sole_track=sole_track)
            if score is not None:
                scored.append((score, ci, ti))
    scored.sort(key=lambda item: item[0], reverse=True)

    used_files: set[int] = set()
    used_tracks: set[int] = set()
    out: list[tuple[_FolderCandidate, ExpectedTrack]] = []
    for _score, ci, ti in scored:
        if ci in used_files or ti in used_tracks:
            continue
        used_files.add(ci)
        used_tracks.add(ti)
        out.append((candidates[ci], expected[ti]))
    return out


def _basename(filename: str) -> str:
    """Last path segment (slskd filenames use backslashes); log basenames not full
    peer paths to keep log lines free of identifying directory structure."""
    return filename.replace("\\", "/").rsplit("/", 1)[-1]


def _row_tier(row: dict) -> str:
    """A library_files row's quality tier, judged exactly like the scanner/gate."""
    return tier_for(row.get("file_format") or "", row.get("bit_rate"))


def _is_strict_upgrade(existing_tier: str, info: AudioInfo) -> bool:
    """Strictly-better only (D4): equal or worse NEVER replaces."""
    return tier_rank(tier_for(info.file_format or "", info.bitrate)) > tier_rank(existing_tier)


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
        download_store: "DownloadStore | None" = None,
        held_dir: Path | None = None,
        recycle_bin: Path | None = None,
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
        # When both are wired, a verify-rejected file is copied here and recorded for an
        # "import anyway" review instead of being silently dropped.
        self._download_store = download_store
        self._held_dir = held_dir
        # Where an upgrade-replaced file is MOVED instead of deleted (D4/D19). None
        # disables replace-on-import entirely - an upgrade must never destroy the
        # only copy of the old bytes.
        self._recycle_bin = recycle_bin

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

    async def process_downloaded_folder(
        self, manifest: DownloadManifest, files: list[Path]
    ) -> ProcessResult:
        """Import an UNPACKED Usenet folder (D18). Unlike the slskd path, the filenames
        are unknown up front (often obfuscated) and the per-track tags may be ENTIRELY
        ABSENT (verified against a real rip: only ``album`` was set), so this matches
        each on-disk file to the manifest's expected MusicBrainz tracklist by
        **duration** (the one always-available signal), with tagged track/title/MBID and
        the filename track number as tie-breakers. Only files that match a tracklist
        position import; the rest (bonus tracks not in MB, scene samples, a merged-track
        file) are dropped (owner Q1). The matched MB track supplies the metadata stamped
        onto the file, since the file's own tags can't be trusted."""
        if self._naming is None or self._library is None or not self._library_paths:
            raise RuntimeError("FileProcessor is not configured for downloads")

        expected = manifest.expected_tracks
        if not expected:
            logger.warning("process.folder_no_tracklist", extra={"task_id": manifest.task_id})
            return ProcessResult(succeeded=[], failed=[])

        # Read tags+info for each candidate file (off the loop); an unreadable file is
        # not a track. ``info.duration_seconds`` is present even when metadata tags aren't.
        candidates: list[_FolderCandidate] = []
        for path in files:
            try:
                tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
            except Exception:  # noqa: BLE001 - unreadable -> not an importable track
                logger.info("process.folder_unreadable", extra={"file": _basename(str(path))})
                continue
            candidates.append(_FolderCandidate(path=path, tag=tag, info=info))

        # Import-time wrong-album guard (#1, tagless-safe): an obfuscated release name can slip
        # the grab-time guard, but the unpacked files' album tag reveals the truth. Reject the
        # WHOLE folder (-> 0 imported -> the orchestrator blocklists it by identity + fails over)
        # rather than import a different album's tracks that happen to match the tracklist by
        # duration. Skipped when no file carries an album tag (relies on duration matching then).
        if _folder_names_wrong_album(candidates, manifest.artist_name, manifest.album_title):
            logger.info(
                "process.folder_wrong_album",
                extra={
                    "task_id": manifest.task_id,
                    "files": len(candidates),
                    "album": manifest.album_title,
                },
            )
            return ProcessResult(succeeded=[], failed=[])

        matches = _match_folder_to_tracklist(candidates, expected)
        logger.info(
            "process.folder_matched",
            extra={
                "task_id": manifest.task_id,
                "files": len(candidates),
                "tracklist": len(expected),
                "matched": len(matches),
            },
        )

        succeeded: list[str] = []
        failed: list[FileFailure] = []
        for candidate, track in matches:
            try:
                target = await self._place_matched_file(manifest, candidate, track)
                succeeded.append(str(target))
            except AlreadyImported as already:
                succeeded.append(str(already.path))
            except VerificationFailed as failure:
                failed.append(
                    FileFailure(filename=failure.filename or candidate.path.name, reason=failure.reason)
                )
                logger.info(
                    "process.folder_verify_failed",
                    extra={"task_id": manifest.task_id, "file": _basename(candidate.path.name), "reason": failure.reason},
                )

        if succeeded:
            parents = list({Path(p).parent for p in succeeded})
            try:
                await self._library.reconcile_with_filesystem(targets=parents)
            except Exception:  # noqa: BLE001 - reconcile is best-effort
                logger.warning("post-import reconcile failed for task %s", manifest.task_id)
        return ProcessResult(succeeded=succeeded, failed=failed)

    async def _place_matched_file(
        self, manifest: DownloadManifest, candidate: "_FolderCandidate", track: "ExpectedTrack"
    ) -> Path:
        """Tag (from the matched MB track) -> position-dedup -> verify -> move -> insert.
        Duration already gated the match, so re-checking it here would be a tautology -
        AcoustID is the optional recording-identity backstop."""
        source, tag, info = candidate.path, candidate.tag, candidate.info
        target_tag = self._build_folder_target_tag(manifest, track, tag)
        target_path = self._library_paths[0] / self._naming.format_path(
            manifest.naming_template, target_tag, info.file_format
        )

        # Position-dedup seam: an upgrade import that strictly beats the occupying
        # file falls THROUGH (so the AcoustID check below still runs, D10) and
        # retires the old file after publishing; anything else keeps the existing
        # copy exactly as before.
        # EXCEPTION (P4 livelock fix, R3): an occupying row that does NOT cover the
        # matched MB track is a squatter from an earlier wrong grab, not a duplicate
        # of this verified file - import alongside, keep the squatter for review (D5).
        # Upgrade runs are exempt: replace-at-position is their OWNED semantics
        # (CollectionMgmt D4/D18), and the strictly-better rule already governs.
        replace_old: Path | None = None
        if target_tag.track_number:
            present = await self._library.get_file_at_position(
                manifest.release_group_mbid, target_tag.disc_number or 1, target_tag.track_number
            )
            occupied_by_other = (
                manifest.origin != "upgrade"
                and present is not None
                and not row_covers_track(
                    present,
                    recording_mbid=track.recording_mbid,
                    title=track.title,
                    duration_seconds=track.duration_seconds,
                )
            )
            if occupied_by_other:
                logger.info(
                    "process.position_occupied_by_noncovering",
                    extra={
                        "task_id": manifest.task_id,
                        "disc": target_tag.disc_number or 1,
                        "track": target_tag.track_number,
                        "occupying_path": present.get("file_path"),
                    },
                )
            elif present is not None and present.get("file_path") != str(target_path):
                replace_old = self._position_upgrade_target(manifest.origin, present, info)
                if replace_old is None:
                    try:
                        source.unlink()
                    except OSError:
                        logger.warning("Could not remove duplicate source %s", source)
                    return Path(present["file_path"])

        fp = None
        if self._verify_downloads and self._fingerprinter is not None:
            fp = await self._fingerprinter.fingerprint(source)
            if _fingerprint_disagrees(fp, track, manifest.artist_name):
                await self._hold_for_review(
                    source=source, manifest=manifest,
                    reason="fingerprint_mismatch",
                    evidence_title=getattr(fp, "title", None),
                    evidence_artist=getattr(fp, "artist", None),
                    evidence_score=getattr(fp, "score", None),
                    track_number=track.track_number, disc_number=track.disc_number or 1,
                    track_title=track.title, recording_mbid=track.recording_mbid,
                    duration_seconds=info.duration_seconds, file_format=info.file_format,
                )
                raise VerificationFailed(
                    "AcoustID identified a different recording",
                    reason="fingerprint_mismatch", filename=source.name,
                )

        try:
            target_path.parent.mkdir(parents=True, exist_ok=True)
            if target_path.exists():
                if await self._same_path_upgrade_applies(manifest.origin, target_path, info):
                    await self._replace_same_path(source, target_path, target_tag)
                else:
                    try:
                        source.unlink()
                    except OSError:
                        logger.warning("Could not remove leftover source %s", source)
            else:
                await asyncio.to_thread(self._import_into_library, source, target_path, target_tag)
            # Retire on BOTH branches: a crash between publish and retire leaves the
            # target present on the re-run, and the old file must still be replaced
            # (else two active rows share one (disc, track) slot).
            if replace_old is not None:
                await self._retire_replaced_file(replace_old)
            await self._library.upsert_file(
                target_path, target_tag, info,
                release_group_mbid=manifest.release_group_mbid,
                release_mbid=manifest.release_mbid,
                recording_mbid=target_tag.musicbrainz_recording_id,
                confidence=_import_confidence(
                    tag=tag, info=info, expected_track=track,
                    canonical_duration=track.duration_seconds, fp=fp,
                ),
                source="download",
                download_task_id=manifest.task_id, source_path=str(source),
            )
        except Exception as exc:  # noqa: BLE001 - import I/O or DB error -> per-file failure
            raise VerificationFailed(
                f"Import failed for {source.name}: {exc}", reason=IMPORT_FAILED, filename=source.name
            ) from exc
        return target_path

    # --- Replace-on-import (CollectionManagement D4/D18/D19) --------------------
    # Fires ONLY for an origin='upgrade' import, and only strictly-better. Two
    # shapes: same-path (mp3_192 -> mp3_320, identical filename - recycle BEFORE the
    # in-place publish) and different-path (mp3 -> flac - publish, soft-delete the
    # old row, recycle the old file). Everything else keeps today's add-only skips.

    def _position_upgrade_target(self, origin: str, present: dict, info: AudioInfo) -> Path | None:
        """The occupied slot's old file path when this upgrade import may replace it
        (strictly better + a recycle bin to preserve the old bytes), else ``None``
        (the caller keeps the existing file - today's dedup behaviour)."""
        if origin != "upgrade" or self._recycle_bin is None:
            return None
        if _is_strict_upgrade(_row_tier(present), info):
            return Path(present["file_path"])
        return None

    async def _existing_tier_at(self, path: Path) -> str | None:
        """Tier of the file currently at ``path``: the library row when it has one,
        else the file's own tags. ``None`` = undeterminable - the caller must then
        KEEP the existing file (never replace what we can't judge)."""
        rows = await self._library.get_attributions_for_paths([str(path)])
        row = rows.get(str(path))
        if row is not None and row.get("file_format"):
            return _row_tier(row)
        try:
            _tag, info = await asyncio.to_thread(self._tagger.read_tags, path)
        except Exception:  # noqa: BLE001 - unreadable existing file -> don't touch it
            return None
        return tier_for(info.file_format or "", info.bitrate)

    async def _same_path_upgrade_applies(
        self, origin: str, target_path: Path, info: AudioInfo
    ) -> bool:
        if origin != "upgrade" or self._recycle_bin is None:
            return False
        existing_tier = await self._existing_tier_at(target_path)
        return existing_tier is not None and _is_strict_upgrade(existing_tier, info)

    async def _replace_same_path(
        self, source: Path, target_path: Path, target_tag: AudioTag
    ) -> None:
        """Same-path replace: recycle the current file BEFORE the in-place
        ``os.replace`` publish (publishing first would destroy the old bytes with
        nothing left to recycle); restore it if the import then fails."""
        recycled = await asyncio.to_thread(recycle, target_path, self._recycle_bin)
        logger.info(
            "process.upgrade_replaced",
            extra={"target": target_path.name, "recycled_to": str(recycled)},
        )
        try:
            await asyncio.to_thread(self._import_into_library, source, target_path, target_tag)
        except BaseException:
            try:
                await asyncio.to_thread(shutil.move, str(recycled), str(target_path))
            except OSError:
                logger.error(
                    "Upgrade import failed AND the old file could not be restored; "
                    "it is preserved at %s", recycled,
                )
            raise

    async def _retire_replaced_file(self, old_path: Path) -> None:
        """Different-path replace, after the new file is published: soft-delete the
        old row, then recycle the old file (order per plan - the row must go first
        so a crash between the two can't leave an active row for a recycled file)."""
        await self._library.soft_delete_file(str(old_path))
        try:
            await asyncio.to_thread(recycle, old_path, self._recycle_bin)
            logger.info("process.upgrade_replaced", extra={"replaced": str(old_path)})
        except OSError as exc:
            # Import succeeded and the row is gone; a stranded old file may be
            # re-adopted by a later reconcile - surface it rather than fail the import.
            logger.warning("Upgrade-replaced file %s could not be recycled: %s", old_path, exc)

    async def _hold_for_review(
        self,
        *,
        source: Path,
        manifest: DownloadManifest,
        reason: str,
        evidence_title: str | None,
        evidence_artist: str | None,
        evidence_score: float | None,
        track_number: int | None,
        disc_number: int,
        track_title: str | None,
        recording_mbid: str | None,
        duration_seconds: float | None,
        file_format: str | None,
    ) -> None:
        """Copy a verify-rejected file into the held area and record it for an "import anyway"
        review. The verifier said the audio/tags aren't the expected recording, but that's
        frequently just wrong crowd metadata - so rather than drop the track, we keep it for
        a human decision. Evidence fields carry what the rejecting check SAW (AcoustID's
        identification for ``fingerprint_mismatch``, the file's own tags for
        ``tag_mismatch``, the measured length for ``wrong_track``). Fully fail-open: any
        error here just means it isn't held (exactly today's behaviour), never a broken
        import."""
        if self._download_store is None or self._held_dir is None:
            return
        try:
            task = await self._download_store.get_task(manifest.task_id)
            if task is None:
                return
            self._held_dir.mkdir(parents=True, exist_ok=True)
            held_path = self._held_dir / f"{uuid4().hex}_{source.name}"
            await asyncio.to_thread(shutil.copy2, source, held_path)
            held_id = await self._download_store.record_held_import(
                user_id=task.user_id,
                held_path=str(held_path),
                reason=reason,
                source=task.source or "soulseek",
                origin=manifest.origin,
                source_task_id=manifest.task_id,
                release_group_mbid=manifest.release_group_mbid,
                release_mbid=manifest.release_mbid,
                recording_mbid=recording_mbid,
                track_number=track_number,
                disc_number=disc_number,
                track_title=track_title,
                artist_name=manifest.artist_name,
                artist_mbid=manifest.artist_mbid,
                album_title=manifest.album_title,
                year=manifest.year,
                original_filename=source.name,
                file_format=file_format,
                duration_seconds=duration_seconds,
                evidence_title=evidence_title,
                evidence_artist=evidence_artist,
                evidence_score=evidence_score,
                naming_template=manifest.naming_template,
            )
            if held_id is None:
                # this track is already held (we failed over through another edition) - drop
                # the extra copy so the held area keeps exactly one candidate per track
                held_path.unlink(missing_ok=True)
            else:
                logger.info(
                    "process.held_for_review",
                    extra={
                        "task_id": manifest.task_id,
                        "track": track_title,
                        "reason": reason,
                        "evidence_title": evidence_title,
                        "evidence_artist": evidence_artist,
                    },
                )
        except Exception as exc:  # noqa: BLE001 - holding is best-effort
            logger.warning("Could not hold %s for review: %s", source.name, exc)

    async def place_held_file(self, held: "HeldImport") -> Path:
        """Force-import a held file under the track it was matched to, WITHOUT the AcoustID
        identity check (a human has judged it correct). Stamps the album's MBIDs onto the file
        so a later rescan trusts it (tag tier) and never re-rejects. Raises ``FileNotFoundError``
        if the held file is gone, or on import I/O error - the caller maps that to a 4xx."""
        source = Path(held.held_path)
        if not source.exists():
            raise FileNotFoundError(held.held_path)
        tag, info = await asyncio.to_thread(self._tagger.read_tags, source)
        target_tag = AudioTag(
            title=held.track_title or tag.title or "",
            artist=held.artist_name or tag.artist or "",
            album=held.album_title or tag.album or "",
            album_artist=held.artist_name,
            track_number=held.track_number,
            disc_number=held.disc_number or 1,
            year=held.year,
            genre=tag.genre,
            musicbrainz_release_group_id=held.release_group_mbid,
            musicbrainz_release_id=held.release_mbid,
            musicbrainz_recording_id=held.recording_mbid or tag.musicbrainz_recording_id,
            musicbrainz_artist_id=tag.musicbrainz_artist_id,
            musicbrainz_album_artist_id=held.artist_mbid or tag.musicbrainz_album_artist_id,
        )
        target_path = self._library_paths[0] / self._naming.format_path(
            held.naming_template or "", target_tag, info.file_format
        )
        # D10 confirm-replace: an upgrade's held file (AcoustID disagreed, a human
        # judged it correct) performs the same strictly-better replace as a normal
        # upgrade import. Non-upgrade held imports keep today's behaviour exactly.
        origin = held.origin
        if origin == "user" and self._download_store is not None and held.source_task_id:
            # legacy rows (held before origin was persisted) fall back to the task,
            # which may since have been cleared - the persisted column is authoritative
            task = await self._download_store.get_task(held.source_task_id)
            if task is not None:
                origin = task.origin
        replace_old: Path | None = None
        if origin == "upgrade" and held.track_number and held.release_group_mbid:
            present = await self._library.get_file_at_position(
                held.release_group_mbid, held.disc_number or 1, held.track_number
            )
            if present is not None and present.get("file_path") != str(target_path):
                replace_old = self._position_upgrade_target(origin, present, info)
                if replace_old is None:
                    # equal/worse never replaces (D4) - keep the existing copy
                    source.unlink(missing_ok=True)
                    return Path(present["file_path"])
        target_path.parent.mkdir(parents=True, exist_ok=True)
        if target_path.exists():
            if await self._same_path_upgrade_applies(origin, target_path, info):
                await self._replace_same_path(source, target_path, target_tag)
            else:
                source.unlink(missing_ok=True)
        else:
            await asyncio.to_thread(self._import_into_library, source, target_path, target_tag)
        # retire on both branches (see _place_matched_file: crash-rerun safety)
        if replace_old is not None:
            await self._retire_replaced_file(replace_old)
        await self._library.upsert_file(
            target_path,
            target_tag,
            info,
            release_group_mbid=held.release_group_mbid,
            release_mbid=held.release_mbid,
            recording_mbid=target_tag.musicbrainz_recording_id,
            # Deliberately 1.0 (P7/D8): a human reviewed the evidence and said
            # "import anyway" - that attribution decision outranks every heuristic,
            # and the rescanner must never second-guess it.
            confidence=1.0,
            source="download",
            download_task_id=held.source_task_id,
            source_path=held.held_path,
        )
        return target_path

    @staticmethod
    def _build_folder_target_tag(
        manifest: DownloadManifest, track: "ExpectedTrack", file_tag: AudioTag
    ) -> AudioTag:
        """Target tag for a folder-matched file: identity comes from the matched MB
        track (the file's own tags may be empty), album identity from the manifest."""
        return AudioTag(
            title=track.title or file_tag.title or "",
            artist=file_tag.artist or manifest.artist_name or "",
            album=manifest.album_title,
            album_artist=manifest.artist_name,
            track_number=track.track_number,
            disc_number=track.disc_number or 1,
            year=manifest.year,
            genre=file_tag.genre,
            musicbrainz_release_group_id=manifest.release_group_mbid,
            musicbrainz_release_id=manifest.release_mbid,
            musicbrainz_recording_id=track.recording_mbid or file_tag.musicbrainz_recording_id,
            musicbrainz_artist_id=file_tag.musicbrainz_artist_id,
            musicbrainz_album_artist_id=manifest.artist_mbid,
            acoustid_id=file_tag.acoustid_id,
            compilation=file_tag.compilation,
        )

    async def _process_one(
        self, expected: ExpectedFile, manifest: DownloadManifest
    ) -> Path:
        """Verify -> tag -> move -> insert one file. Raises ``VerificationFailed``
        (per-file) or ``AlreadyImported`` (crash-idempotency)."""
        source = await self._client.get_file_path(
            manifest.handle, expected.filename, expected.size
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

        expected_track = (
            manifest.expected_tracks[0] if len(manifest.expected_tracks) == 1 else None
        )

        # Position-level dedup, before the expensive verification below: if the album
        # already holds a file at this (disc, track), don't write a second copy. Stops
        # the flac-vs-mp3 / failover re-pull duplicate (completeness dedupes by position,
        # but the files themselves never were) and skips fingerprinting a copy we
        # discard. Known track numbers only; an untagged file (track 0) falls through to
        # the path check below.
        # An upgrade import that strictly beats the occupying file falls through (the
        # fingerprint check below still runs first, D10) and retires the old file
        # after publishing; anything else keeps the existing copy exactly as before.
        # EXCEPTION (P4 livelock fix, incident review R3): when the occupying row does
        # NOT cover the expected track - a wrong file from an earlier grab squatting on
        # the slot - the verified new file is not its duplicate. Import alongside (the
        # naming template yields a distinct filename); the squatter stays for human
        # review (D5: never auto-delete). Without this, the correct re-download was
        # unlinked as a "duplicate" of the wrong file, forever. Upgrade runs are
        # exempt: replace-at-position is their owned semantics (CollectionMgmt D4/D18).
        replace_old: Path | None = None
        if target_tag.track_number:
            present = await self._library.get_file_at_position(
                manifest.release_group_mbid,
                target_tag.disc_number or 1,
                target_tag.track_number,
            )
            occupied_by_other = (
                manifest.origin != "upgrade"
                and expected_track is not None
                and present is not None
                and not row_covers_track(
                    present,
                    recording_mbid=expected_track.recording_mbid,
                    title=expected_track.title,
                    duration_seconds=expected_track.duration_seconds,
                )
            )
            if occupied_by_other:
                logger.info(
                    "process.position_occupied_by_noncovering",
                    extra={
                        "task_id": manifest.task_id,
                        "file": _basename(expected.filename),
                        "disc": target_tag.disc_number or 1,
                        "track": target_tag.track_number,
                        "occupying_path": present.get("file_path"),
                    },
                )
            elif present is not None and present.get("file_path") != str(target_path):
                replace_old = self._position_upgrade_target(manifest.origin, present, info)
                if replace_old is None:
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
            # A relaxed re-pull (every candidate failed this gate - the MB length is
            # suspect) captures the closest match for HUMAN review instead of
            # importing it silently with the gate off (D9) or discarding it.
            if manifest.hold_on_wrong_track:
                await self._hold_for_review(
                    source=source, manifest=manifest,
                    reason=WRONG_TRACK,
                    evidence_title=tag.title,
                    evidence_artist=tag.artist,
                    evidence_score=None,
                    track_number=tag.track_number, disc_number=tag.disc_number or 1,
                    track_title=(expected_track.title if expected_track else tag.title),
                    recording_mbid=(
                        expected_track.recording_mbid if expected_track
                        else tag.musicbrainz_recording_id
                    ),
                    duration_seconds=info.duration_seconds, file_format=info.file_format,
                )
            raise VerificationFailed(
                f"Duration mismatch ({info.duration_seconds:.0f}s vs "
                f"{expected.duration:.0f}s)",
                reason=WRONG_TRACK if manifest.is_track else "duration_mismatch",
                filename=expected.filename,
            )
        # The file's OWN tags vs the requested identity (P2): catches a tagged-wrong
        # file even when AcoustID has no coverage (its fail-open hole - the 2026-07-05
        # incident file was tagged "Dan Romer / Arrival in Ashford"). Runs before the
        # fingerprint so a clearly-mislabelled file never spends an AcoustID call.
        if self._verify_downloads and _tag_conflict_reason(
            tag, info, manifest, expected_track
        ):
            await self._hold_for_review(
                source=source, manifest=manifest,
                reason="tag_mismatch",
                evidence_title=tag.title,
                evidence_artist=tag.artist,
                evidence_score=None,
                track_number=tag.track_number, disc_number=tag.disc_number or 1,
                track_title=(expected_track.title if expected_track else tag.title),
                recording_mbid=(
                    expected_track.recording_mbid if expected_track
                    else tag.musicbrainz_recording_id
                ),
                duration_seconds=info.duration_seconds, file_format=info.file_format,
            )
            raise VerificationFailed(
                "File tags name different content than requested",
                reason="tag_mismatch",
                filename=expected.filename,
            )
        # AcoustID recording-identity check, only when verify is on and a fingerprinter is
        # wired. fail-open: the fingerprinter never raises, a non-pass/empty result skips.
        # When the manifest carries the single expected track (a track download or a
        # 1-track single), its TITLE is checked too; otherwise only a confidently
        # different ARTIST is rejected. NOT a release-group check - that false-rejects
        # valid reissue/compilation tracks whose AcoustID RG coverage is incomplete.
        fp = None
        if self._verify_downloads and self._fingerprinter is not None:
            fp = await self._fingerprinter.fingerprint(source)
            if _fingerprint_disagrees(fp, expected_track, manifest.artist_name):
                await self._hold_for_review(
                    source=source, manifest=manifest,
                    reason="fingerprint_mismatch",
                    evidence_title=getattr(fp, "title", None),
                    evidence_artist=getattr(fp, "artist", None),
                    evidence_score=getattr(fp, "score", None),
                    track_number=tag.track_number, disc_number=tag.disc_number or 1,
                    track_title=tag.title, recording_mbid=tag.musicbrainz_recording_id,
                    duration_seconds=info.duration_seconds, file_format=info.file_format,
                )
                raise VerificationFailed(
                    "AcoustID identified a different recording",
                    reason="fingerprint_mismatch",
                    filename=expected.filename,
                )

        # mutating phase (stage -> tag -> publish -> insert): an I/O or DB error must
        # fail just this file, not abort the album and orphan files imported earlier
        try:
            target_path.parent.mkdir(parents=True, exist_ok=True)

            if target_path.exists():
                if await self._same_path_upgrade_applies(manifest.origin, target_path, info):
                    # _import_into_library consumes the source + prunes its dirs
                    await self._replace_same_path(source, target_path, target_tag)
                else:
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

            # retire on both branches (see _place_matched_file: crash-rerun safety)
            if replace_old is not None:
                await self._retire_replaced_file(replace_old)

            await self._library.upsert_file(
                target_path,
                target_tag,
                info,
                release_group_mbid=manifest.release_group_mbid,
                release_mbid=manifest.release_mbid,
                recording_mbid=tag.musicbrainz_recording_id,
                confidence=_import_confidence(
                    tag=tag, info=info, expected_track=expected_track,
                    # expected.duration is the CANONICAL length only when the strict
                    # gate was armed (is_track); a peer-advertised length verifies
                    # nothing about identity.
                    canonical_duration=expected.duration if manifest.is_track else None,
                    fp=fp,
                ),
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
                # cross-mount: copy bytes, then best-effort metadata. copy2's next step
                # (copystat -> chmod/utime) is rejected by some filesystems even for the
                # file's owner (TrueNAS NFSv4 ACLs), so it threw and killed the import.
                # Bytes are all that matter; the tag write below resets mtime, so swallow.
                shutil.copyfile(source, tmp)
                try:
                    shutil.copystat(source, tmp)
                except OSError:
                    logger.debug("copystat skipped for %s (filesystem rejected metadata)", tmp.name)
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
