"""AudioFingerprinter - Tier-3 identification via fpcalc (chromaprint) + AcoustID.

Wraps the ``fpcalc`` subprocess and the AcoustID lookup HTTP API. The httpx
client, decrypted AcoustID API key, and AcoustID rate limiter are all supplied by
``get_audio_fingerprinter`` (AUD-12 / AUD-2) - this class never acquires them.
The api key arrives as a *callable* read fresh on each call, so changing it in
settings takes effect without a restart and without coupling this infrastructure
module to the preferences service.

Fail-open: every failure path (no key, missing binary, subprocess/network error)
returns a ``FingerprintResult`` whose ``status`` the scanner treats as "skip
Tier 3, queue for manual review". Fingerprinting never raises into the scan.
"""

import asyncio
import logging
import os
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any

import httpx

from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from models.audio import FingerprintResult

logger = logging.getLogger(__name__)

# AcoustID recommends >=120s of audio, so ``-length`` caps the fingerprint window at
# 120s. On files SHORTER than this, fpcalc reads to EOF and exits non-zero
# ("Error decoding audio frame (End of file)") *after* emitting a valid FINGERPRINT=
# line - see ``_run_fpcalc``, which tolerates that exit rather than pre-reading duration.
_FPCALC_LENGTH = "120"
_FPCALC_TIMEOUT = 30.0
# Upper bound on concurrent fpcalc subprocesses, core-scaled below. fpcalc is an external
# subprocess (escapes the GIL), so more cores => genuinely more parallel fingerprinting; the
# cap keeps a many-core host from a wide subprocess fan-out, and the downstream AcoustID HTTP
# limiter (3/s) bounds end-to-end throughput regardless. 4 matches the signed core-scaled default.
_MAX_FPCALC_CONCURRENCY = 4
# A best result below this AcoustID score is not a confident match.
_ACOUSTID_MIN_SCORE = 0.70
# Separators that delimit multiple artists in an AcoustID credit string.
_ARTIST_SEPARATORS = (";", ",", "feat.", "ft.", "&", "+", "vs.", " x ", " with ")


class FingerprintStatus:
    PASS = "pass"
    FAIL = "fail"
    SKIP = "skip"
    DISABLED = "disabled"
    ERROR = "error"


def split_artist_credit(credit: str) -> list[str]:
    """Split an AcoustID artist-credit string into individual artist tokens.

    The token primitive for a forthcoming compilation artist-match step (per
    plan §"Multi-artist & compilation handling"): a target artist is matched
    against *any* token, so aggressive splitting is intentional. Not yet wired
    into the scanner - Tier 3 currently keys only on the recording MBID.
    """
    tokens = [credit]
    for separator in _ARTIST_SEPARATORS:
        split_tokens: list[str] = []
        for token in tokens:
            split_tokens.extend(token.split(separator))
        tokens = split_tokens
    return [token.strip() for token in tokens if token.strip()]


class AudioFingerprinter:
    ACOUSTID_API = "https://api.acoustid.org/v2/lookup"

    def __init__(
        self,
        http: httpx.AsyncClient,
        api_key_provider: Callable[[], str],
        rate_limiter: TokenBucketRateLimiter,
    ) -> None:
        self._http = http
        self._api_key_provider = api_key_provider
        self._rate_limiter = rate_limiter
        # Gate concurrent fpcalc subprocesses so a scan can't fork-bomb the host; core-scaled
        # (see _MAX_FPCALC_CONCURRENCY).
        self._fpcalc_semaphore = asyncio.Semaphore(min(os.cpu_count() or 2, _MAX_FPCALC_CONCURRENCY))

    async def fingerprint(self, path: Path) -> FingerprintResult:
        api_key = self._api_key_provider()
        if not api_key:
            return FingerprintResult(status=FingerprintStatus.DISABLED)

        try:
            fingerprint, duration = await self._run_fpcalc(path)
        except (OSError, subprocess.SubprocessError, asyncio.TimeoutError, ValueError) as exc:
            logger.warning("fpcalc failed for %s: %s", path, exc)
            return FingerprintResult(status=FingerprintStatus.ERROR, error=str(exc))

        await self._rate_limiter.acquire()
        try:
            response = await self._http.post(
                self.ACOUSTID_API,
                data={
                    "client": api_key,
                    "duration": str(duration),
                    "fingerprint": fingerprint,
                    "meta": "recordings releasegroups",
                },
            )
            response.raise_for_status()
            payload = response.json()
        except (httpx.HTTPError, ValueError) as exc:
            logger.warning("AcoustID lookup failed for %s: %s", path, exc)
            return FingerprintResult(status=FingerprintStatus.ERROR, error=str(exc))

        return self._parse_response(payload)

    async def _run_fpcalc(self, path: Path) -> tuple[str, int]:
        async with self._fpcalc_semaphore:
            # NOT ``-raw``: AcoustID's /v2/lookup expects the COMPRESSED (base64) Chromaprint
            # fingerprint that plain fpcalc emits. ``-raw`` emits comma-separated integers,
            # which the API rejects with HTTP 400 (every lookup was silently failing).
            proc = await asyncio.create_subprocess_exec(
                "fpcalc", "-length", _FPCALC_LENGTH, str(path),
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
            try:
                stdout, stderr = await asyncio.wait_for(
                    proc.communicate(), timeout=_FPCALC_TIMEOUT
                )
            except asyncio.TimeoutError:
                proc.kill()
                await proc.wait()
                raise
            output = stdout.decode("utf-8", "ignore")
            # fpcalc exits non-zero ("Error decoding audio frame (End of file)") on tracks
            # shorter than ``-length`` seconds, but still writes a valid FINGERPRINT= line to
            # stdout. Only treat a non-zero exit as a real failure when NO fingerprint was
            # produced; otherwise use the fingerprint it emitted so sub-120s tracks still match.
            if proc.returncode != 0:
                stderr_text = stderr.decode("utf-8", "ignore").strip()
                if "FINGERPRINT=" not in output:
                    raise subprocess.CalledProcessError(
                        proc.returncode or -1, "fpcalc", stderr=stderr_text
                    )
                # Tolerated non-zero exit (typically the sub-120s EOF case). Preserve
                # fpcalc's stderr so a changed/unexpected error is still visible, and
                # record that a short-track fingerprint was used for observability.
                logger.warning(
                    "fpcalc exited %s but emitted a fingerprint for %s; using it (stderr: %s)",
                    proc.returncode,
                    path,
                    stderr_text or "<empty>",
                )
            return self._parse_fpcalc_output(output)

    @staticmethod
    def _parse_fpcalc_output(output: str) -> tuple[str, int]:
        """Parse ``fpcalc -raw`` output: a ``DURATION=`` line and a
        ``FINGERPRINT=`` line. Each is matched by prefix so the ``FINGERPRINT=``
        label never leaks into the payload."""
        duration = 0
        fingerprint = ""
        for line in output.strip().split("\n"):
            if line.startswith("DURATION="):
                duration = int(float(line.split("=", 1)[1]))
            elif line.startswith("FINGERPRINT="):
                fingerprint = line.split("=", 1)[1]
        if not fingerprint:
            raise ValueError("fpcalc output missing FINGERPRINT line")
        if duration <= 0:
            # A zero/missing duration would make AcoustID return an empty result
            # set, which is indistinguishable from a genuine no-match. Surface it as
            # an error (logged) instead of a silent skip.
            raise ValueError("fpcalc output missing or non-positive DURATION")
        return fingerprint, duration

    def _parse_response(self, payload: dict[str, Any]) -> FingerprintResult:
        # See https://acoustid.org/webservice
        if payload.get("status") != "ok":
            return FingerprintResult(
                status=FingerprintStatus.ERROR, error=str(payload.get("status"))
            )
        results = payload.get("results") or []
        if not results:
            return FingerprintResult(status=FingerprintStatus.SKIP)
        best = results[0]
        score = best.get("score") or 0.0
        if score < _ACOUSTID_MIN_SCORE:
            return FingerprintResult(status=FingerprintStatus.SKIP, score=score)
        recordings = best.get("recordings") or []
        if not recordings or not recordings[0].get("id"):
            # Confident audio match, but nothing to key the library row on.
            return FingerprintResult(status=FingerprintStatus.FAIL, score=score)
        recording = recordings[0]
        artists = recording.get("artists") or []
        artist = "; ".join(a.get("name", "") for a in artists if a.get("name")) or None
        return FingerprintResult(
            status=FingerprintStatus.PASS,
            score=score,
            recording_id=recording.get("id"),
            title=recording.get("title"),
            artist=artist,
            duration=recording.get("duration"),
            release_group_ids=self._extract_release_group_ids(recording, best),
        )

    @staticmethod
    def _extract_release_group_ids(
        recording: dict[str, Any], best: dict[str, Any]
    ) -> list[str]:
        """Collect release-group MBIDs from the ``meta=recordings releasegroups``
        payload. AcoustID nests release groups under the recording; some payloads
        also carry them at the result level, so both are merged (deduped, order
        preserved). Used by the download-verify release-group check (D15/B2)."""
        ids: list[str] = []
        for source in (recording.get("releasegroups"), best.get("releasegroups")):
            for rg in source or []:
                rg_id = rg.get("id")
                if rg_id and rg_id not in ids:
                    ids.append(rg_id)
        return ids
