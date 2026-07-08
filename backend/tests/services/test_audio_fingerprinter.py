"""Tests for AudioFingerprinter - fpcalc + AcoustID Tier-3 identification.

Mocks at the boundary: ``asyncio.create_subprocess_exec`` stands in for the
fpcalc binary, and an injected httpx-like client stands in for the AcoustID API.
The real fpcalc binary and the network are never touched.
"""

import asyncio
import os
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest

from infrastructure.audio.fingerprinter import (
    _MAX_FPCALC_CONCURRENCY,
    AudioFingerprinter,
    FingerprintStatus,
    split_artist_credit,
)
from infrastructure.resilience.rate_limiter import TokenBucketRateLimiter

_FP_OK = b"DURATION=183\nFINGERPRINT=AQADtMmSaEkSRYkG\n"


class _FakeProc:
    def __init__(self, *, stdout=b"", stderr=b"", returncode=0, delay=0.0, concurrency=None):
        self._stdout = stdout
        self._stderr = stderr
        self.returncode = returncode
        self._delay = delay
        self._concurrency = concurrency

    async def communicate(self):
        if self._concurrency is not None:
            self._concurrency["now"] += 1
            self._concurrency["max"] = max(self._concurrency["max"], self._concurrency["now"])
        if self._delay:
            await asyncio.sleep(self._delay)
        if self._concurrency is not None:
            self._concurrency["now"] -= 1
        return self._stdout, self._stderr

    def kill(self):
        pass

    async def wait(self):
        return self.returncode


def _patch_fpcalc(monkeypatch, *, stdout=_FP_OK, returncode=0, delay=0.0, concurrency=None, raises=None):
    async def fake_exec(*args, **kwargs):
        if raises is not None:
            raise raises
        return _FakeProc(stdout=stdout, returncode=returncode, delay=delay, concurrency=concurrency)

    monkeypatch.setattr(asyncio, "create_subprocess_exec", fake_exec)


def _acoustid_response(payload):
    resp = MagicMock()
    resp.raise_for_status = MagicMock()
    resp.json = MagicMock(return_value=payload)
    return resp


def _http_client(payload=None, *, post_raises=None):
    http = MagicMock()
    if post_raises is not None:
        http.post = AsyncMock(side_effect=post_raises)
    else:
        http.post = AsyncMock(return_value=_acoustid_response(payload))
    return http


def _pass_payload(score=0.95, rec_id="rec-1", title="Airbag", artists=None):
    artists = artists if artists is not None else [{"name": "Radiohead"}]
    return {
        "status": "ok",
        "results": [
            {
                "score": score,
                "recordings": [
                    {"id": rec_id, "title": title, "artists": artists, "duration": 180}
                ],
            }
        ],
    }


def _make(http, *, key="acoustid-key", rate_limiter=None):
    rl = rate_limiter or TokenBucketRateLimiter(rate=1000.0, capacity=1000)
    return AudioFingerprinter(http, lambda: key, rl)


@pytest.mark.asyncio
async def test_pass_returns_recording_match(monkeypatch):
    _patch_fpcalc(monkeypatch)
    fp = _make(_http_client(_pass_payload(score=0.95)))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.PASS
    assert res.score == 0.95
    assert res.recording_id == "rec-1"
    assert res.title == "Airbag"
    assert res.artist == "Radiohead"


@pytest.mark.asyncio
async def test_pass_surfaces_release_group_ids(monkeypatch):
    # download-verify (D15/B2) needs release_group_ids from the meta=recordings
    # releasegroups lookup, else the branch is dead
    _patch_fpcalc(monkeypatch)
    payload = {
        "status": "ok",
        "results": [
            {
                "score": 0.95,
                "recordings": [
                    {
                        "id": "rec-1",
                        "title": "Airbag",
                        "artists": [{"name": "Radiohead"}],
                        "duration": 180,
                        "releasegroups": [{"id": "rg-1"}, {"id": "rg-2"}],
                    }
                ],
            }
        ],
    }
    fp = _make(_http_client(payload))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.PASS
    assert res.release_group_ids == ["rg-1", "rg-2"]


@pytest.mark.asyncio
async def test_pass_release_group_ids_empty_when_absent(monkeypatch):
    _patch_fpcalc(monkeypatch)
    fp = _make(_http_client(_pass_payload(score=0.95)))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.PASS
    assert res.release_group_ids == []


@pytest.mark.asyncio
async def test_skip_when_score_below_floor(monkeypatch):
    _patch_fpcalc(monkeypatch)
    fp = _make(_http_client(_pass_payload(score=0.5)))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.SKIP
    assert res.score == 0.5
    assert res.recording_id is None


@pytest.mark.asyncio
async def test_skip_when_no_results(monkeypatch):
    _patch_fpcalc(monkeypatch)
    fp = _make(_http_client({"status": "ok", "results": []}))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.SKIP


@pytest.mark.asyncio
async def test_fail_when_confident_but_no_recording_id(monkeypatch):
    _patch_fpcalc(monkeypatch)
    payload = {"status": "ok", "results": [{"score": 0.92, "recordings": []}]}
    fp = _make(_http_client(payload))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.FAIL
    assert res.score == 0.92
    assert res.recording_id is None


@pytest.mark.asyncio
async def test_disabled_when_no_api_key(monkeypatch):
    def boom(*args, **kwargs):
        raise AssertionError("fpcalc must not run without an API key")

    monkeypatch.setattr(asyncio, "create_subprocess_exec", boom)
    rl = MagicMock()
    rl.acquire = AsyncMock()
    fp = AudioFingerprinter(_http_client(_pass_payload()), lambda: "", rl)
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.DISABLED
    rl.acquire.assert_not_awaited()


@pytest.mark.asyncio
async def test_error_when_fpcalc_missing(monkeypatch):
    _patch_fpcalc(monkeypatch, raises=FileNotFoundError("fpcalc"))
    fp = _make(_http_client(_pass_payload()))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.ERROR
    assert res.error


@pytest.mark.asyncio
async def test_error_when_fpcalc_nonzero_exit(monkeypatch):
    _patch_fpcalc(monkeypatch, returncode=2, stdout=b"")
    fp = _make(_http_client(_pass_payload()))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.ERROR


@pytest.mark.asyncio
async def test_nonzero_exit_with_fingerprint_still_passes(monkeypatch):
    # fpcalc exits non-zero ("End of file") on sub-120s tracks but still emits a
    # valid FINGERPRINT= line; the emitted fingerprint must be used, not discarded.
    _patch_fpcalc(monkeypatch, returncode=2, stdout=_FP_OK)
    http = _http_client(_pass_payload())
    fp = _make(http)
    res = await fp.fingerprint(Path("/short.flac"))
    assert res.status == FingerprintStatus.PASS
    _, kwargs = http.post.call_args
    assert kwargs["data"]["fingerprint"] == "AQADtMmSaEkSRYkG"


@pytest.mark.asyncio
async def test_error_when_fpcalc_output_has_no_fingerprint(monkeypatch):
    _patch_fpcalc(monkeypatch, stdout=b"DURATION=100\n")
    fp = _make(_http_client(_pass_payload()))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.ERROR


@pytest.mark.asyncio
async def test_error_when_acoustid_http_fails(monkeypatch):
    _patch_fpcalc(monkeypatch)
    fp = _make(_http_client(post_raises=httpx.ConnectError("boom")))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.ERROR


@pytest.mark.asyncio
async def test_error_when_acoustid_status_not_ok(monkeypatch):
    _patch_fpcalc(monkeypatch)
    fp = _make(_http_client({"status": "error", "error": {"message": "invalid client"}}))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.status == FingerprintStatus.ERROR


@pytest.mark.asyncio
async def test_fpcalc_output_parsed_into_acoustid_post(monkeypatch):
    _patch_fpcalc(monkeypatch, stdout=b"DURATION=183\nFINGERPRINT=AQADtMmSaEkSRYkG\n")
    http = _http_client(_pass_payload())
    fp = _make(http)
    await fp.fingerprint(Path("/x.flac"))
    _, kwargs = http.post.call_args
    data = kwargs["data"]
    assert data["duration"] == "183"  # parsed from DURATION= line, not mutagen
    assert data["fingerprint"] == "AQADtMmSaEkSRYkG"  # FINGERPRINT= prefix stripped
    assert data["meta"] == "recordings releasegroups"
    assert data["client"] == "acoustid-key"


@pytest.mark.asyncio
async def test_rate_limiter_awaited_before_http(monkeypatch):
    _patch_fpcalc(monkeypatch)
    order: list[str] = []
    rl = MagicMock()
    rl.acquire = AsyncMock(side_effect=lambda *a, **k: order.append("acquire"))
    http = MagicMock()

    async def post(*args, **kwargs):
        order.append("post")
        return _acoustid_response(_pass_payload())

    http.post = AsyncMock(side_effect=post)
    fp = AudioFingerprinter(http, lambda: "k", rl)
    await fp.fingerprint(Path("/x.flac"))
    assert order == ["acquire", "post"]
    rl.acquire.assert_awaited_once()


@pytest.mark.asyncio
async def test_semaphore_gates_concurrent_fpcalc(monkeypatch):
    # fpcalc concurrency is core-scaled (Tier 2a) but capped, so a scan uses more than one
    # core for fingerprinting without fork-bombing the host. Launch more tasks than the cap
    # and assert the semaphore never lets more than the cap run at once.
    expected_cap = min(os.cpu_count() or 2, _MAX_FPCALC_CONCURRENCY)
    concurrency = {"now": 0, "max": 0}
    _patch_fpcalc(monkeypatch, delay=0.02, concurrency=concurrency)
    fp = _make(_http_client(_pass_payload()))
    await asyncio.gather(
        *[fp.fingerprint(Path(f"/{i}.flac")) for i in range(expected_cap + 3)]
    )
    assert concurrency["max"] == expected_cap


def test_split_artist_credit_semicolon():
    assert split_artist_credit("Artist A; Artist B") == ["Artist A", "Artist B"]


def test_split_artist_credit_mixed_separators():
    assert split_artist_credit("A feat. B & C") == ["A", "B", "C"]


def test_split_artist_credit_single_artist():
    assert split_artist_credit("Radiohead") == ["Radiohead"]


@pytest.mark.asyncio
async def test_pass_joins_multiple_artist_credit(monkeypatch):
    _patch_fpcalc(monkeypatch)
    payload = _pass_payload(artists=[{"name": "Artist A"}, {"name": "Artist B"}])
    fp = _make(_http_client(payload))
    res = await fp.fingerprint(Path("/x.flac"))
    assert res.artist == "Artist A; Artist B"
    assert split_artist_credit(res.artist) == ["Artist A", "Artist B"]


@pytest.mark.asyncio
async def test_submits_compressed_fingerprint_not_raw(monkeypatch):
    # Regression: fpcalc must NOT run with -raw. The raw (comma-separated integer)
    # fingerprint makes AcoustID's /v2/lookup return HTTP 400 - every lookup was silently
    # failing. The COMPRESSED fingerprint plain fpcalc emits must be the one POSTed.
    captured = []

    async def fake_exec(*args, **kwargs):
        captured.append(args)
        return _FakeProc(stdout=_FP_OK, returncode=0)

    monkeypatch.setattr(asyncio, "create_subprocess_exec", fake_exec)
    http = _http_client(_pass_payload())
    res = await _make(http).fingerprint(Path("/x.flac"))

    assert res.status == FingerprintStatus.PASS
    assert captured[0][0] == "fpcalc"
    assert "-raw" not in captured[0]                       # the bug: no -raw flag
    assert http.post.call_args.kwargs["data"]["fingerprint"] == "AQADtMmSaEkSRYkG"
