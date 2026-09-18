"""NewznabReleaseScorer: category-primary quality, obfuscation-tolerant identity
(Q4 auto-accept), quality gating, quarantine-by-identity."""

import time
from unittest.mock import AsyncMock

import pytest

from api.v1.schemas.settings import DownloadPolicySettings
from models.download import TargetAlbum
from models.download_identity import usenet_identity
from repositories.protocols.indexer import UsenetRelease
from services.native.acquisition.decision import SpecPolicy
from services.native.acquisition.quality import build_snapshot
from services.native.newznab_release_scorer import NewznabReleaseScorer

_TARGET = TargetAlbum(artist_name="Radiohead", album_title="In Rainbows", track_count=10)


def _store(quarantine=None):
    store = AsyncMock()
    store.load_quarantine_set.return_value = set(quarantine or set())
    return store


def _release(
    title, cats, *, size=600_000_000, grabs=200, guid="g", password=0, usenet_date=None
) -> UsenetRelease:
    return UsenetRelease(
        indexer_id="ds", indexer_name="DS", guid=guid, title=title,
        nzb_url="https://idx/nzb", size_bytes=size, category_ids=cats, grabs=grabs,
        password=password, usenet_date=usenet_date,
    )


def _scorer(quarantine=None):
    return NewznabReleaseScorer(_store(quarantine))


def policy_snapshot(**over):
    """Legacy quality kwargs moved onto the required keyword-only snapshot."""
    base = dict(quality_min="mp3_320", quality_max="lossless")
    base.update(over)
    return build_snapshot(DownloadPolicySettings(**base))


@pytest.mark.asyncio
async def test_clean_lossless_release_reaches_auto():
    rel = _release("Radiohead - In Rainbows (2007) [FLAC]", [3040])
    [cand] = await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot())
    assert cand.source == "usenet"
    assert cand.tier == "auto"
    assert cand.final_score >= 0.70


@pytest.mark.asyncio
async def test_obfuscated_title_good_category_and_grabs_reaches_auto():
    # Q4: a scrambled name collapses title-identity + title-quality, but category 3040
    # (lossless) + indexer-match + healthy grabs still get it to auto. This is what
    # keeps D3's automatic Usenet fallback actually firing.
    rel = _release("aHR0cHM6 scrambled xQ.part01.rar", [3040], grabs=205)
    [cand] = await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot())
    assert cand.tier == "auto", cand.final_score


@pytest.mark.asyncio
async def test_obfuscated_title_without_grabs_is_only_manual():
    rel = _release("aHR0cHM6 scrambled xQ.part01.rar", [3040], grabs=None)
    [cand] = await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot())
    assert cand.tier == "manual"


@pytest.mark.asyncio
async def test_music_video_category_is_dropped():
    rel = _release("Radiohead - In Rainbows [music video]", [3020])
    assert await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_quality_min_lossless_drops_mp3_release():
    rel = _release("Radiohead - In Rainbows [MP3-320]", [3010])
    scorer = _scorer()
    assert (
        await scorer.rank(
            _TARGET,
            [rel],
            snapshot=policy_snapshot(quality_min="lossless", quality_max="lossless"),
        )
        == []
    )


@pytest.mark.asyncio
async def test_unparseable_title_unknown_quality_not_hard_failed():
    # No category, no quality hint -> "unknown" quality, still scored (not dropped);
    # the import tag-match is the real gate.
    rel = _release("????", [], grabs=10)
    scored = await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot())
    assert len(scored) == 1


@pytest.mark.asyncio
async def test_quarantined_identity_is_filtered():
    rel = _release("Radiohead - In Rainbows [FLAC]", [3040], size=600_000_000)
    ident = usenet_identity(rel.title, rel.size_bytes)
    scored = await _scorer(quarantine={("usenet", ident)}).rank(
        _TARGET, [rel], snapshot=policy_snapshot()
    )
    assert scored == []


@pytest.mark.asyncio
async def test_tiny_lossless_release_is_rejected():
    # A 20MB "FLAC" for a 10-track album (~2MB/track) can't be the full album - reject it
    # before download (Lidarr's runtime-based AcceptableSize, now applied to lossless too).
    rel = _release("Radiohead - In Rainbows [FLAC]", [3040], size=20_000_000, grabs=None)
    assert await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_password_protected_release_is_rejected():
    # SABnzbd can't auto-unpack a password-protected NZB - drop it before download.
    rel = _release("Radiohead - In Rainbows [FLAC]", [3040], password=1)
    assert await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_unknown_password_status_is_not_rejected():
    # Unknown is not evidence that the release is password-protected.
    rel = _release("Radiohead - In Rainbows [FLAC]", [3040], password=-1)
    assert len(await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot())) == 1


@pytest.mark.asyncio
async def test_implausibly_small_lossy_release_is_rejected():
    # A "320" release ~10MB can't hold a 10-track album at 320kbps (~96MB) - it's
    # truncated/fake/sample. Reject before download (Lidarr AcceptableSize).
    rel = _release("Radiohead - In Rainbows [MP3-320]", [3010], size=10_000_000)
    assert await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_normal_sized_lossy_release_is_kept():
    rel = _release("Radiohead - In Rainbows [MP3-320]", [3010], size=90_000_000)
    scored = await _scorer().rank(_TARGET, [rel], snapshot=policy_snapshot())
    assert len(scored) == 1


@pytest.mark.asyncio
async def test_live_release_rejected_for_a_studio_album():
    # The Led Zeppelin case: a Live EP must not match the requested studio album.
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="Led Zeppelin", year=1969, track_count=9)
    rel = _release("Led Zeppelin - 2025 - Live (EP) (16-bit FLAC + MP3)", [3040])
    assert await _scorer().rank(target, [rel], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_boxset_and_compilation_rejected():
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="Led Zeppelin", year=1969, track_count=9)
    boxset = _release('Led Zeppelin Definitive Collection (2008 SHMCD boxset)-1969-Led Zeppelin [14/30] - "09 How Many More Times.flac"', [3040])
    assert await _scorer().rank(target, [boxset], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_live_request_keeps_live_release():
    # Gated on the requested title: if the user asked for a live album, "live" is allowed.
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="How the West Was Won (Live)", year=2003, track_count=23)
    rel = _release("Led Zeppelin - How the West Was Won (Live) [FLAC]", [3040], size=1_500_000_000)
    assert len(await _scorer().rank(target, [rel], snapshot=policy_snapshot())) == 1


@pytest.mark.asyncio
async def test_complete_recordings_boxset_rejected_by_name():
    # A multi-album boxset is rejected by NAME (Lidarr's discography handling), not size -
    # lossless size varies too much to cap.
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="Led Zeppelin", year=1969, track_count=9)
    rel = _release("Led Zeppelin - The Complete Studio Recordings [FLAC]", [3040], size=4_509_338_173)
    assert await _scorer().rank(target, [rel], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_numbered_sequel_rejected_for_self_titled_debut():
    # The real bug: the self-titled debut "Led Zeppelin" kept matching "Led Zeppelin II"
    # releases (token_set_ratio ignores the extra "II"). This is the exact failing title.
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="Led Zeppelin", year=1969, track_count=9)
    rel = _release("led_zeppelin-led_zeppelin_ii-lp-32bit-wavpack-1969-reetkever [FLAC]", [3040])
    assert await _scorer().rank(target, [rel], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_requested_numbered_album_keeps_its_own_volume():
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="Led Zeppelin IV", year=1971, track_count=8)
    rel = _release("Led Zeppelin - Led Zeppelin IV (1971) [FLAC]", [3040], size=1_500_000_000)
    assert len(await _scorer().rank(target, [rel], snapshot=policy_snapshot())) == 1


@pytest.mark.asyncio
async def test_requested_numbered_album_rejects_a_different_volume():
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="Led Zeppelin IV", year=1971, track_count=8)
    rel = _release("Led Zeppelin - Led Zeppelin II [FLAC]", [3040])
    assert await _scorer().rank(target, [rel], snapshot=policy_snapshot()) == []


@pytest.mark.asyncio
async def test_obfuscated_numbered_album_still_passes():
    # Q4: requesting a numbered album must NOT reject an obfuscated release that just doesn't
    # spell the numeral - and a RAR ``partNN`` token must never read as a volume marker.
    target = TargetAlbum(artist_name="Led Zeppelin", album_title="Led Zeppelin IV", year=1971, track_count=8)
    rel = _release("aHR0cHM6 scrambled xQ.part01.rar", [3040], grabs=205)
    assert len(await _scorer().rank(target, [rel], snapshot=policy_snapshot())) == 1


@pytest.mark.asyncio
async def test_hires_release_breaks_tie_over_redbook_lossless():
    # H1: two equal-score lossless releases (same identity + grabs) - the 24/96 one ranks
    # first via the hi-res title tie-breaker. token_set_ratio ignores the extra "24 96"
    # tokens, so identity stays equal and the tie engages.
    redbook = _release("Radiohead - In Rainbows [FLAC]", [3040], guid="rb", grabs=200)
    hires = _release("Radiohead - In Rainbows [24-96 FLAC]", [3040], guid="hr", grabs=200)
    scored = await _scorer().rank(_TARGET, [redbook, hires], snapshot=policy_snapshot())
    assert [c.final_score for c in scored][0] == scored[1].final_score  # genuine tie
    assert scored[0].usenet_release.guid == "hr"  # hi-res wins the tie


@pytest.mark.asyncio
async def test_ignored_term_policy_drops_release():
    # A user ignored-term (a format token the always-on guards ignore) drops the release.
    rel = _release("Radiohead - In Rainbows WEB FLAC", [3040])
    scorer = NewznabReleaseScorer(_store())
    assert (
        await scorer.rank(
            _TARGET,
            [rel],
            snapshot=policy_snapshot(),
            spec_extras=SpecPolicy(ignored_terms=("web",)),
        )
        == []
    )
    assert (
        len(await NewznabReleaseScorer(_store()).rank(_TARGET, [rel], snapshot=policy_snapshot()))
        == 1
    )  # kept without the policy


@pytest.mark.asyncio
async def test_max_size_policy_drops_oversize_release():
    rel = _release("Radiohead - In Rainbows [FLAC]", [3040], size=5_000_000_000)  # 5GB boxset-sized
    scorer = NewznabReleaseScorer(_store())
    assert (
        await scorer.rank(
            _TARGET,
            [rel],
            snapshot=policy_snapshot(),
            spec_extras=SpecPolicy(max_size_mb=1000),
        )
        == []
    )


@pytest.mark.asyncio
async def test_retention_policy_drops_release_past_retention():
    rel = _release("Radiohead - In Rainbows [FLAC]", [3040], usenet_date=time.time() - 100 * 86400)
    scorer = NewznabReleaseScorer(_store())
    assert (
        await scorer.rank(
            _TARGET,
            [rel],
            snapshot=policy_snapshot(),
            spec_extras=SpecPolicy(usenet_retention_days=30),
        )
        == []
    )
    # within retention it's kept
    fresh = _release("Radiohead - In Rainbows [FLAC]", [3040], usenet_date=time.time() - 5 * 86400)
    kept = await NewznabReleaseScorer(_store()).rank(
        _TARGET,
        [fresh],
        snapshot=policy_snapshot(),
        spec_extras=SpecPolicy(usenet_retention_days=30),
    )
    assert len(kept) == 1


@pytest.mark.asyncio
async def test_year_in_title_outranks_edition_without_it():
    # The original-year edition should win over a remaster when the user asked for the
    # original year (edition disambiguation).
    target = TargetAlbum(artist_name="Pink Floyd", album_title="Animals", year=1977, track_count=5)
    original = _release("Pink Floyd - Animals (1977) [FLAC]", [3040], guid="a")
    remaster = _release("Pink Floyd - Animals (2018 Remix) [FLAC]", [3040], guid="b")
    scored = await _scorer().rank(target, [remaster, original], snapshot=policy_snapshot())
    assert scored[0].usenet_release.guid == "a"
    assert scored[0].final_score > scored[1].final_score
