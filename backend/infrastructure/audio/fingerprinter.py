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
import hashlib
import math
import logging
import os
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any

import httpx

from core.exceptions import ExternalServiceError, RateLimitedError
from infrastructure.http.deduplication import RequestDeduplicator
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter
from infrastructure.resilience.retry import (
    CircuitBreaker,
    CircuitOpenError,
    with_retry,
)
from infrastructure.service_health import report_breaker_health
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
_ACOUSTID_MALFORMED_RESPONSE = "malformed AcoustID response"
_ARTIST_SEPARATORS = (";", ",", "feat.", "ft.", "&", "+", "vs.", " x ", " with ")

# F-040: the house resilience pattern (audiodb/coverart/geocoding precedent) -
# transient network/5xx failures retry with backoff through a named breaker so
# an AcoustID outage short-circuits the fan-out instead of paying fpcalc+HTTP
# per track; 429 honors Retry-After; 4xx is deterministic and non-retriable.
_acoustid_circuit_breaker = CircuitBreaker(
    failure_threshold=5,
    success_threshold=2,
    timeout=60.0,
    name="acoustid",
    on_state_change=report_breaker_health(
        "acoustid",
        "metadata",
        message="AcoustID, our Tier-3 fingerprint identification source, is "
        "having trouble - identification falls back to review for now.",
    ),
)
# F-045: identical concurrent lookups (same audio across roots) coalesce like
# every other external repository client.
acoustid_deduplicator = RequestDeduplicator()


class AcoustIDRejectedError(ValueError):
    """Deterministic 4xx rejection (bad key / malformed request). Never
    retried and never counted toward the circuit breaker."""


class FingerprintMemo:
    """F-043: bounded process-level memo of chromaprint results keyed by a
    cheap content proxy - sha256 over the first 64 KiB plus the file size.
    Collisions only AVOID work; they never assert identity (every consumer
    still verifies recording MBIDs downstream), which the plan explicitly
    blesses. Restart clears it by design."""

    def __init__(self, max_entries: int = 256) -> None:
        self._max_entries = max_entries
        self._entries: dict[str, tuple[str, int, bool]] = {}
        self._order: list[str] = []

    @staticmethod
    def content_key(path: Path) -> str:
        with open(path, "rb") as handle:
            prefix = handle.read(64 * 1024)
        size = path.stat().st_size
        digest = hashlib.sha256(prefix)
        digest.update(str(size).encode("ascii"))
        return digest.hexdigest()

    def get(self, key: str) -> tuple[str, int, bool] | None:
        return self._entries.get(key)

    def put(self, key: str, value: tuple[str, int, bool]) -> None:
        if key in self._entries:
            return
        while len(self._order) >= self._max_entries:
            evicted = self._order.pop(0)
            self._entries.pop(evicted, None)
        self._entries[key] = value
        self._order.append(key)


fingerprint_memo = FingerprintMemo()


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


def _retry_after_seconds(header: str | None) -> float:
    """AcoustID communicates pacing via Retry-After seconds; fall back to the
    historical 60s window when the header is absent or unparseable."""
    if header:
        try:
            value = float(header.strip())
            if value > 0:
                return value
        except ValueError:
            pass
    return 60.0


def _echo_partial(result: FingerprintResult) -> FingerprintResult:
    values = {
        field: getattr(result, field)
        for field in (
            "status",
            "score",
            "recording_id",
            "recording_ids",
            "title",
            "artist",
            "duration",
            "error",
            "release_group_ids",
        )
    }
    values["partial_decode"] = True
    return FingerprintResult(**values)


def _echo_duration(result: FingerprintResult, duration: int) -> FingerprintResult:
    values = {
        field: getattr(result, field)
        for field in (
            "status",
            "score",
            "recording_id",
            "recording_ids",
            "title",
            "artist",
            "duration",
            "error",
            "release_group_ids",
        )
    }
    values["duration"] = duration
    return FingerprintResult(**values)


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
        self._fpcalc_semaphore = asyncio.Semaphore(
            min(os.cpu_count() or 2, _MAX_FPCALC_CONCURRENCY)
        )

    async def fingerprint(self, path: Path) -> FingerprintResult:
        if not self.is_enabled():
            return FingerprintResult(status=FingerprintStatus.DISABLED)

        try:
            fingerprint, duration, partial = await self._generate_tracked(path)
        except (
            OSError,
            subprocess.SubprocessError,
            asyncio.TimeoutError,
            ValueError,
        ) as exc:
            logger.warning("fpcalc failed for %s: %s", path, exc)
            return FingerprintResult(status=FingerprintStatus.ERROR, error=str(exc))

        result = await self.lookup_fingerprint(fingerprint, duration)
        if partial:
            result = _echo_partial(result)
        return result

    def is_enabled(self) -> bool:
        return bool(self._api_key_provider())

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        fingerprint, duration, _partial = await self._generate_tracked(path)
        return fingerprint, duration

    async def _generate_tracked(self, path: Path) -> tuple[str, int, bool]:
        """F-043/F-044: one chromaprint computation per distinct content key,
        shared by every lane; carries whether the fingerprint came from a
        tolerated PARTIAL decode so callers can demand corroboration."""
        try:
            key = FingerprintMemo.content_key(path)
        except OSError:
            # Unreadable/ephemeral paths: skip the memo, let fpcalc surface
            # the real failure as before.
            fingerprint, duration = await self._run_fpcalc(path)
            return fingerprint, duration, False
        cached = fingerprint_memo.get(key)
        if cached is not None:
            return cached
        fingerprint, duration = await self._run_fpcalc(path)
        partial = getattr(self, "_last_generation_partial", False)
        self._last_generation_partial = False
        fingerprint_memo.put(key, (fingerprint, duration, partial))
        return fingerprint, duration, partial

    async def lookup_fingerprint(
        self, fingerprint: str, duration: int
    ) -> FingerprintResult:
        api_key = self._api_key_provider()
        if not api_key:
            return FingerprintResult(status=FingerprintStatus.DISABLED)
        try:
            payload = await acoustid_deduplicator.dedupe(
                f"{fingerprint}:{duration}",
                lambda: self._lookup_http(fingerprint, duration, api_key),
            )
        except (
            CircuitOpenError,
            RateLimitedError,
            httpx.HTTPError,
            AcoustIDRejectedError,
            ExternalServiceError,
            ValueError,
        ) as exc:
            logger.warning("AcoustID lookup failed: %s", exc)
            return FingerprintResult(status=FingerprintStatus.ERROR, error=str(exc))

        result = self._parse_response(payload)
        if result.status == FingerprintStatus.PASS and result.duration is None:
            # Echo the generation inputs so callers can seed downstream caches
            # without re-reading the file.
            result = _echo_duration(result, duration)
        return result

    @with_retry(
        max_attempts=3,
        base_delay=2.0,
        max_delay=10.0,
        circuit_breaker=_acoustid_circuit_breaker,
        retriable_exceptions=(
            httpx.HTTPError,
            ExternalServiceError,
            RateLimitedError,
        ),
        non_retriable_exceptions=(AcoustIDRejectedError,),
    )
    async def _lookup_http(
        self, fingerprint: str, duration: int, api_key: str
    ) -> dict[str, Any]:
        """One retried, breaker-gated HTTP attempt. The rate limiter is
        awaited inside so EVERY retry attempt re-paces through the 3/s token
        bucket (F-PERF-01 hunt expectation)."""
        await self._rate_limiter.acquire()
        response = await self._http.post(
            self.ACOUSTID_API,
            data={
                "client": api_key,
                "duration": str(duration),
                "fingerprint": fingerprint,
                "meta": "recordings releasegroups",
            },
        )
        if response.status_code == 429:
            retry_after = _retry_after_seconds(response.headers.get("Retry-After"))
            logger.warning(
                "acoustid.ratelimit status=429 retry_after_s=%s", retry_after
            )
            raise RateLimitedError(
                "AcoustID rate limit exceeded", retry_after_seconds=retry_after
            )
        if response.status_code >= 500:
            raise ExternalServiceError(f"AcoustID API error ({response.status_code})")
        if response.status_code != 200:
            raise AcoustIDRejectedError(
                f"AcoustID rejected the lookup ({response.status_code})"
            )
        return response.json()

    async def _run_fpcalc(self, path: Path) -> tuple[str, int]:
        async with self._fpcalc_semaphore:
            # NOT ``-raw``: AcoustID's /v2/lookup expects the COMPRESSED (base64) Chromaprint
            # fingerprint that plain fpcalc emits. ``-raw`` emits comma-separated integers,
            # which the API rejects with HTTP 400 (every lookup was silently failing).
            proc = await asyncio.create_subprocess_exec(
                "fpcalc",
                "-length",
                _FPCALC_LENGTH,
                str(path),
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
                # F-044: mark the result chain so confident matches from a
                # PARTIAL decode can be corroborated downstream.
                self._last_generation_partial = True
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

    def _parse_response(self, payload: Any) -> FingerprintResult:
        # See https://acoustid.org/webservice
        if not isinstance(payload, dict):
            return FingerprintResult(
                status=FingerprintStatus.ERROR,
                error=_ACOUSTID_MALFORMED_RESPONSE,
            )
        status = payload.get("status")
        if not isinstance(status, str):
            return FingerprintResult(
                status=FingerprintStatus.ERROR,
                error=_ACOUSTID_MALFORMED_RESPONSE,
            )
        if status != "ok":
            return FingerprintResult(status=FingerprintStatus.ERROR, error=status)

        raw_results = payload.get("results")
        if raw_results is None:
            results: list[Any] = []
        elif isinstance(raw_results, list):
            results = raw_results
        else:
            return FingerprintResult(
                status=FingerprintStatus.ERROR,
                error=_ACOUSTID_MALFORMED_RESPONSE,
            )
        if not results:
            return FingerprintResult(status=FingerprintStatus.SKIP)

        best = results[0]
        if not isinstance(best, dict):
            return FingerprintResult(
                status=FingerprintStatus.ERROR,
                error=_ACOUSTID_MALFORMED_RESPONSE,
            )
        raw_score = best.get("score")
        if raw_score is None:
            score = 0.0
        elif isinstance(raw_score, bool) or not isinstance(raw_score, (int, float)):
            return FingerprintResult(
                status=FingerprintStatus.ERROR,
                error=_ACOUSTID_MALFORMED_RESPONSE,
            )
        else:
            try:
                score = float(raw_score)
            except (OverflowError, ValueError):
                return FingerprintResult(
                    status=FingerprintStatus.ERROR,
                    error=_ACOUSTID_MALFORMED_RESPONSE,
                )
            if not math.isfinite(score):
                return FingerprintResult(
                    status=FingerprintStatus.ERROR,
                    error=_ACOUSTID_MALFORMED_RESPONSE,
                )
        if score < _ACOUSTID_MIN_SCORE:
            return FingerprintResult(status=FingerprintStatus.SKIP, score=score)

        raw_recordings = best.get("recordings")
        if raw_recordings is None:
            recordings: list[Any] = []
        elif isinstance(raw_recordings, list):
            recordings = raw_recordings
        else:
            return FingerprintResult(
                status=FingerprintStatus.ERROR,
                error=_ACOUSTID_MALFORMED_RESPONSE,
            )
        selected_recording: dict[str, Any] | None = None
        recording_ids: list[str] = []
        seen_recording_ids: set[str] = set()
        for recording in recordings:
            if not isinstance(recording, dict):
                continue
            recording_id = recording.get("id")
            if not isinstance(recording_id, str):
                continue
            recording_id = recording_id.strip()
            normalized_id = recording_id.casefold()
            if not recording_id or normalized_id in seen_recording_ids:
                continue
            seen_recording_ids.add(normalized_id)
            recording_ids.append(recording_id)
            if selected_recording is None:
                selected_recording = recording
        if selected_recording is None:
            # Confident audio match, but nothing to key the library row on.
            return FingerprintResult(status=FingerprintStatus.FAIL, score=score)
        recording = selected_recording

        raw_artists = recording.get("artists")
        artist_names: list[str] = []
        if isinstance(raw_artists, list):
            for artist_entry in raw_artists:
                if not isinstance(artist_entry, dict):
                    continue
                name = artist_entry.get("name")
                if isinstance(name, str):
                    name = name.strip()
                    if name:
                        artist_names.append(name)
        artist = "; ".join(artist_names) or None

        raw_title = recording.get("title")
        title = raw_title.strip() or None if isinstance(raw_title, str) else None
        raw_duration = recording.get("duration")
        if isinstance(raw_duration, bool):
            duration = None
        elif isinstance(raw_duration, int):
            duration = raw_duration
        elif (
            isinstance(raw_duration, float)
            and math.isfinite(raw_duration)
            and raw_duration.is_integer()
        ):
            duration = int(raw_duration)
        else:
            duration = None
        return FingerprintResult(
            status=FingerprintStatus.PASS,
            score=score,
            recording_id=recording_ids[0],
            title=title,
            artist=artist,
            duration=duration,
            release_group_ids=self._extract_release_group_ids(recording, best),
            recording_ids=recording_ids,
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
        if not isinstance(recording, dict) or not isinstance(best, dict):
            return ids
        seen_ids: set[str] = set()
        for source in (recording.get("releasegroups"), best.get("releasegroups")):
            if not isinstance(source, list):
                continue
            for release_group in source:
                if not isinstance(release_group, dict):
                    continue
                release_group_id = release_group.get("id")
                if not isinstance(release_group_id, str):
                    continue
                release_group_id = release_group_id.strip()
                if not release_group_id or release_group_id in seen_ids:
                    continue
                seen_ids.add(release_group_id)
                ids.append(release_group_id)
        return ids
